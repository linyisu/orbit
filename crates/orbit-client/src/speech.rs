//! Client-host speech recognition. Only model downloads use the network;
//! microphone samples stay in bounded memory and never enter workspace transport.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig};
use std::sync::mpsc::{self, Receiver, SyncSender};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
};
pub use tokio_util::sync::CancellationToken;

pub const SAMPLE_RATE_HZ: usize = 16_000;
pub const MAX_RECORDING_SECONDS: usize = 120;
const MODEL_BYTES: u64 = 237_202_501;
const MODEL_URL: &str = "https://huggingface.co/csukuangfj/\
    sherpa-onnx-streaming-paraformer-bilingual-zh-en/resolve/\
    8e40c43232a1c5c66c82111efc5820d3accca11b";
const MODEL_FILES: [(&str, u64, &str); 3] = [
    (
        "encoder.int8.onnx",
        165_462_184,
        "81a70226a8934e6ed92aa1d4fc486b428b5398e2f2619ed4897b7294cab90e9a",
    ),
    (
        "decoder.int8.onnx",
        71_664_561,
        "f3cca9f77bb9d93c8fcbfb63ae617b6b1ee96818df3aa3b151c40658fe38594f",
    ),
    (
        "tokens.txt",
        75_756,
        "59aba8873a2ed1e122c25fee421e25f283b63290efbde85c1f01a853d83cb6e6",
    ),
];
const AUDIO_CHUNK_SAMPLES: usize = 1_600;
const AUDIO_QUEUE_CHUNKS: usize = 32;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// Model download progress; a zero total means preparation has not started downloading.
#[derive(Clone, Copy, Default)]
pub struct Progress {
    pub received_bytes: u64,
    pub total_bytes: u64,
}

/// The global client-host cache, independent of the active local or remote project.
pub fn model_directory() -> Result<PathBuf> {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .context("无法确定本地语音模型目录，请设置 XDG_CACHE_HOME")?;
    ensure!(root.is_absolute(), "XDG_CACHE_HOME 必须是绝对路径");
    Ok(root.join("orbit/speech/paraformer-zh-en-int8"))
}

/// Prepare a verified multilingual model and load it off the UI thread.
/// Cancellation removes incomplete downloads and never replaces a usable cache file.
pub async fn prepare(
    directory: PathBuf,
    cancel: CancellationToken,
    progress: watch::Sender<Progress>,
) -> Result<Arc<Recognizer>> {
    tokio::fs::create_dir_all(&directory).await?;
    let mut completed = 0;
    for (name, bytes, hash) in MODEL_FILES {
        let path = directory.join(name);
        if !verified(&path, bytes, hash, &cancel).await? {
            download(
                &path,
                &format!("{MODEL_URL}/{name}"),
                bytes,
                hash,
                &cancel,
                &progress,
                Progress {
                    received_bytes: completed,
                    total_bytes: MODEL_BYTES,
                },
            )
            .await?;
        }
        completed += bytes;
        progress.send_replace(Progress {
            received_bytes: completed,
            total_bytes: MODEL_BYTES,
        });
    }
    ensure!(!cancel.is_cancelled(), "语音准备已取消");
    progress.send_replace(Progress {
        received_bytes: MODEL_BYTES,
        total_bytes: MODEL_BYTES,
    });
    tokio::task::spawn_blocking(move || {
        ensure!(!cancel.is_cancelled(), "语音准备已取消");
        let mut config = OnlineRecognizerConfig::default();
        config.model_config.paraformer.encoder = Some(
            directory
                .join("encoder.int8.onnx")
                .to_str()
                .context("模型路径不是 UTF-8")?
                .to_owned(),
        );
        config.model_config.paraformer.decoder = Some(
            directory
                .join("decoder.int8.onnx")
                .to_str()
                .context("模型路径不是 UTF-8")?
                .to_owned(),
        );
        config.model_config.tokens = Some(
            directory
                .join("tokens.txt")
                .to_str()
                .context("模型路径不是 UTF-8")?
                .to_owned(),
        );
        config.model_config.num_threads = 2;
        config.decoding_method = Some("greedy_search".into());
        config.enable_endpoint = true;
        config.rule1_min_trailing_silence = 2.4;
        config.rule2_min_trailing_silence = 1.2;
        config.rule3_min_utterance_length = 20.;
        let engine = OnlineRecognizer::create(&config).context("无法加载本地中文流式模型")?;
        ensure!(!cancel.is_cancelled(), "语音准备已取消");
        Ok(Arc::new(Recognizer { engine }))
    })
    .await?
}

async fn verified(path: &Path, bytes: u64, hash: &str, cancel: &CancellationToken) -> Result<bool> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if file.metadata().await?.len() != bytes {
        return Ok(false);
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 64 * 1_024];
    loop {
        let count = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("语音准备已取消"),
            count = file.read(&mut buffer) => count?,
        };
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        == hash)
}

async fn download(
    path: &Path,
    url: &str,
    bytes: u64,
    hash: &str,
    cancel: &CancellationToken,
    progress: &watch::Sender<Progress>,
    base: Progress,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()?;
    let mut response = tokio::select! {
        biased;
        _ = cancel.cancelled() => anyhow::bail!("模型下载已取消"),
        response = client.get(url).send() => response?.error_for_status()?,
    };
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length == bytes),
        "语音模型下载大小不符"
    );
    let pending = tempfile::NamedTempFile::new_in(path.parent().context("模型目录无效")?)?;
    let mut file = tokio::fs::File::from_std(pending.reopen()?);
    let mut received = 0_u64;
    let mut digest = Sha256::new();
    let mut percent = 0;
    progress.send_replace(Progress {
        received_bytes: base.received_bytes,
        total_bytes: if base.total_bytes == 0 {
            bytes
        } else {
            base.total_bytes
        },
    });
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("模型下载已取消"),
            chunk = response.chunk() => chunk?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        received += chunk.len() as u64;
        ensure!(received <= bytes, "语音模型下载超出预期大小");
        digest.update(&chunk);
        file.write_all(&chunk).await?;
        let next = received * 100 / bytes;
        if next != percent {
            percent = next;
            progress.send_replace(Progress {
                received_bytes: base.received_bytes + received,
                total_bytes: if base.total_bytes == 0 {
                    bytes
                } else {
                    base.total_bytes
                },
            });
        }
    }
    ensure!(
        received == bytes
            && digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                == hash,
        "语音模型校验失败，请重试下载"
    );
    file.sync_all().await?;
    drop(file);
    ensure!(!cancel.is_cancelled(), "模型下载已取消");
    pending
        .persist(path)
        .map_err(|error| error.error)?;
    Ok(())
}

/// Loaded CPU streaming recognizer, shared across client-host windows.
pub struct Recognizer {
    engine: OnlineRecognizer,
}

/// Bounded microphone input. Dropping it ends the input; cancellation discards all hypotheses.
pub struct Capture {
    sender: SyncSender<Vec<f32>>,
    samples: usize,
}

impl Capture {
    /// Queue short PCM chunks without blocking the UI. Overrun fails explicitly rather than
    /// silently dropping samples and producing an incomplete transcript.
    pub fn push(&mut self, samples: &[i16]) -> Result<()> {
        ensure!(
            samples.len() <= SAMPLE_RATE_HZ * MAX_RECORDING_SECONDS - self.samples,
            "录音最长为 {MAX_RECORDING_SECONDS} 秒，请分段输入"
        );
        for chunk in samples.chunks(AUDIO_CHUNK_SAMPLES) {
            self.sender
                .try_send(
                    chunk
                        .iter()
                        .map(|sample| *sample as f32 / 32_768.)
                        .collect(),
                )
                .context("本地语音处理未跟上录音或已停止，请缩短录音后重试")?;
        }
        self.samples += samples.len();
        Ok(())
    }
}

/// One streaming session. Run on a worker; updates contain the replaceable full hypothesis.
pub struct Recognition {
    engine: Arc<Recognizer>,
    receiver: Receiver<Vec<f32>>,
    cancel: CancellationToken,
    updates: watch::Sender<String>,
}

impl Recognizer {
    /// Create independent input and worker ends. Input is bounded to 3.2 seconds of backlog.
    pub fn start(
        self: &Arc<Self>,
        cancel: CancellationToken,
        updates: watch::Sender<String>,
    ) -> (Capture, Recognition) {
        let (sender, receiver) = mpsc::sync_channel(AUDIO_QUEUE_CHUNKS);
        (
            Capture { sender, samples: 0 },
            Recognition {
                engine: self.clone(),
                receiver,
                cancel,
                updates,
            },
        )
    }
}

impl Recognition {
    /// Decode incrementally until the input closes. No full recording is retained or replayed.
    /// Cancellation is checked between decode steps and while waiting for microphone input.
    pub fn run(self) -> Result<String> {
        let stream = self.engine.engine.create_stream();
        let recognizer = &self.engine.engine;
        let mut committed = String::new();
        let mut last = String::new();
        loop {
            ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
            match self
                .receiver
                .recv_timeout(Duration::from_millis(100))
            {
                Ok(samples) => stream.accept_waveform(SAMPLE_RATE_HZ as i32, &samples),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            while recognizer.is_ready(&stream) {
                ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
                recognizer.decode(&stream);
            }
            let hypothesis = recognizer
                .get_result(&stream)
                .context("无法读取语音识别结果")?
                .text;
            let text = format!("{committed}{hypothesis}");
            if text != last {
                self.updates.send_replace(text.clone());
                last = text;
            }
            if recognizer.is_endpoint(&stream) {
                committed.push_str(&hypothesis);
                if committed.ends_with(|c: char| c.is_ascii_alphanumeric()) {
                    committed.push(' ');
                }
                recognizer.reset(&stream);
            }
        }
        ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
        // Paraformer needs trailing context to flush words at the recording boundary.
        stream.accept_waveform(SAMPLE_RATE_HZ as i32, &vec![0.; SAMPLE_RATE_HZ * 4 / 5]);
        stream.input_finished();
        while recognizer.is_ready(&stream) {
            ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
            recognizer.decode(&stream);
        }
        ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
        let text = recognizer
            .get_result(&stream)
            .context("无法读取最终语音结果")?
            .text;
        Ok(format!("{committed}{text}")
            .trim()
            .to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_is_bounded_and_preserves_pcm() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut capture = Capture { sender, samples: 0 };
        capture
            .push(&[i16::MIN, 0, i16::MAX])
            .unwrap();
        assert!(capture.push(&[1]).is_err());
        assert_eq!(receiver.recv().unwrap(), [-1., 0., 32_767. / 32_768.]);
        capture.samples = SAMPLE_RATE_HZ * MAX_RECORDING_SECONDS;
        assert!(capture.push(&[1]).is_err());
    }

    #[tokio::test]
    async fn cache_validation_rejects_truncation_corruption_and_cancelled_work() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model.bin");
        let cancel = CancellationToken::new();
        let bytes = b"generated model bytes";
        let hash = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert!(
            !verified(&path, bytes.len() as u64, &hash, &cancel)
                .await
                .unwrap()
        );
        tokio::fs::write(&path, bytes)
            .await
            .unwrap();
        assert!(
            verified(&path, bytes.len() as u64, &hash, &cancel)
                .await
                .unwrap()
        );
        assert!(
            !verified(&path, bytes.len() as u64 + 1, &hash, &cancel)
                .await
                .unwrap()
        );
        tokio::fs::write(&path, vec![0; bytes.len()])
            .await
            .unwrap();
        assert!(
            !verified(&path, bytes.len() as u64, &hash, &cancel)
                .await
                .unwrap()
        );
        cancel.cancel();
        assert!(
            verified(&path, bytes.len() as u64, &hash, &cancel)
                .await
                .is_err()
        );
    }

    async fn serve(body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut connection, _) = listener.accept().await.unwrap();
            let mut request = [0; 4_096];
            let received = connection
                .read(&mut request)
                .await
                .unwrap();
            assert!(received > 0);
            connection
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            connection
                .write_all(&body)
                .await
                .unwrap();
        });
        (url, task)
    }

    #[tokio::test]
    async fn download_installs_only_complete_verified_bytes_and_cleans_partial_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model.bin");
        let bytes = b"generated model payload";
        let hash: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let cancel = CancellationToken::new();
        let (progress, _) = watch::channel(Progress::default());
        tokio::fs::write(&path, b"previous cache")
            .await
            .unwrap();
        for body in [
            bytes[..3].to_vec(),
            vec![0; bytes.len()],
            vec![0; bytes.len() + 1],
        ] {
            let (url, server) = serve(body).await;
            assert!(
                download(
                    &path,
                    &url,
                    bytes.len() as u64,
                    &hash,
                    &cancel,
                    &progress,
                    Progress::default()
                )
                .await
                .is_err()
            );
            server.await.unwrap();
            assert_eq!(tokio::fs::read(&path).await.unwrap(), b"previous cache");
            assert_eq!(
                std::fs::read_dir(directory.path())
                    .unwrap()
                    .count(),
                1
            );
        }
        let (url, server) = serve(bytes.to_vec()).await;
        download(
            &path,
            &url,
            bytes.len() as u64,
            &hash,
            &cancel,
            &progress,
            Progress::default(),
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), bytes);
        assert_eq!(progress.borrow().received_bytes, bytes.len() as u64);
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_stalled_download_and_removes_temporary_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model.bin");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();
        let server = tokio::spawn(async move {
            let (mut connection, _) = listener.accept().await.unwrap();
            let mut request = [0; 4_096];
            let received = connection
                .read(&mut request)
                .await
                .unwrap();
            assert!(received > 0);
            connection
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial")
                .await
                .unwrap();
            server_cancel.cancelled().await;
        });
        let (progress, mut updates) = watch::channel(Progress::default());
        let operation = download(
            &path,
            &url,
            100,
            "unused",
            &cancel,
            &progress,
            Progress::default(),
        );
        let interrupt = async {
            while updates
                .borrow_and_update()
                .received_bytes
                == 0
            {
                updates.changed().await.unwrap();
            }
            cancel.cancel();
        };
        let (result, ()) = tokio::join!(operation, interrupt);
        assert!(result.is_err());
        server.await.unwrap();
        assert_eq!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    #[ignore = "requires verified local models and independently synthesized Mandarin PCM"]
    async fn local_model_streams_generated_mandarin_before_input_finishes() {
        let directory =
            PathBuf::from(std::env::var_os("ORBIT_SPEECH_TEST_MODEL_DIRECTORY").unwrap());
        let pcm = std::fs::read(std::env::var_os("ORBIT_SPEECH_TEST_PCM").unwrap()).unwrap();
        let samples: Vec<_> = pcm
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| i16::from_le_bytes(*sample))
            .collect();
        let (progress, _) = watch::channel(Progress::default());
        let engine = prepare(directory, CancellationToken::new(), progress)
            .await
            .unwrap();
        let (updates, mut partials) = watch::channel(String::new());
        let (mut capture, recognition) = engine.start(CancellationToken::new(), updates);
        let worker = tokio::task::spawn_blocking(move || recognition.run());
        for chunk in samples.chunks(AUDIO_CHUNK_SAMPLES) {
            capture.push(chunk).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            !partials.borrow_and_update().is_empty(),
            "no text before microphone stopped"
        );
        drop(capture);
        let text = worker.await.unwrap().unwrap();
        eprintln!("Mandarin transcript: {text}");
        let expected: Vec<_> = "你好现在开始测试今天下午三点开会请把报告保存在项目目录"
            .chars()
            .collect();
        let actual: Vec<_> = text
            .chars()
            .filter(|c| !c.is_whitespace() && !"，。！？、".contains(*c))
            .collect();
        let mut distance: Vec<_> = (0..=expected.len()).collect();
        for (row, character) in actual.iter().enumerate() {
            let mut diagonal = distance[0];
            distance[0] = row + 1;
            for (column, expected) in expected.iter().enumerate() {
                let previous = distance[column + 1];
                distance[column + 1] = (diagonal + usize::from(character != expected))
                    .min(distance[column] + 1)
                    .min(previous + 1);
                diagonal = previous;
            }
        }
        // Check the full sentence with a 10% character-error ceiling, not just keywords.
        assert!(
            distance[expected.len()] * 10 <= expected.len(),
            "{text}; errors={}",
            distance[expected.len()]
        );
        let (updates, _) = watch::channel(String::new());
        let (mut capture, recognition) = engine.start(CancellationToken::new(), updates);
        capture
            .push(&vec![0; SAMPLE_RATE_HZ])
            .unwrap();
        drop(capture);
        let silence = tokio::task::spawn_blocking(move || recognition.run())
            .await
            .unwrap()
            .unwrap();
        assert!(silence.is_empty(), "silence produced text: {silence}");
        let cancel = CancellationToken::new();
        let (updates, _) = watch::channel(String::new());
        let (_capture, recognition) = engine.start(cancel.clone(), updates);
        let worker = tokio::task::spawn_blocking(move || recognition.run());
        cancel.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), worker)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
}
