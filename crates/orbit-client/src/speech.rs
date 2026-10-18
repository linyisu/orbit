//! Client-host speech recognition. Only model downloads use the network;
//! microphone samples stay in bounded memory and never enter workspace transport.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
};
pub use tokio_util::sync::CancellationToken;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub const SAMPLE_RATE_HZ: usize = 16_000;
pub const MAX_RECORDING_SECONDS: usize = 120;
const MODEL_BYTES: u64 = 147_951_465;
const MODEL_SHA256: &str = "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";
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
    Ok(root.join("orbit/speech"))
}

/// Prepare a verified multilingual model and load it off the UI thread.
/// Cancellation removes incomplete downloads and never replaces a usable cache file.
pub async fn prepare(
    directory: PathBuf,
    cancel: CancellationToken,
    progress: watch::Sender<Progress>,
) -> Result<Arc<Recognizer>> {
    tokio::fs::create_dir_all(&directory).await?;
    let path = directory.join("ggml-base.bin");
    if !verified(&path, MODEL_BYTES, MODEL_SHA256, &cancel).await? {
        download(
            &path,
            MODEL_URL,
            MODEL_BYTES,
            MODEL_SHA256,
            &cancel,
            &progress,
        )
        .await?;
    }
    ensure!(!cancel.is_cancelled(), "语音准备已取消");
    progress.send_replace(Progress {
        received_bytes: MODEL_BYTES,
        total_bytes: MODEL_BYTES,
    });
    tokio::task::spawn_blocking(move || {
        ensure!(!cancel.is_cancelled(), "语音准备已取消");
        let context = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .context("无法加载本地语音模型")?;
        ensure!(!cancel.is_cancelled(), "语音准备已取消");
        Ok(Arc::new(Recognizer { context }))
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
        received_bytes: 0,
        total_bytes: bytes,
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
                received_bytes: received,
                total_bytes: bytes,
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

/// One bounded recording, in the recognizer's 16 kHz mono PCM format.
#[derive(Default)]
pub struct Recording {
    samples: Vec<f32>,
}

impl Recording {
    /// Reject oversized input before allocation; no partial transcript is committed on failure.
    pub fn push(&mut self, samples: &[i16]) -> Result<()> {
        ensure!(
            samples.len() <= SAMPLE_RATE_HZ * MAX_RECORDING_SECONDS - self.samples.len(),
            "录音最长为 {MAX_RECORDING_SECONDS} 秒，请分段输入"
        );
        self.samples.extend(
            samples
                .iter()
                .map(|sample| *sample as f32 / 32_768.),
        );
        Ok(())
    }
}

/// Loaded CPU recognizer. Call `transcribe` on a worker, never on the UI thread.
pub struct Recognizer {
    context: WhisperContext,
}

impl Recognizer {
    /// Recognize without translation or prior-session context. Cancellation aborts inference;
    /// neither cancelled nor failed work returns text for insertion into a draft.
    pub fn transcribe(&self, recording: Recording, cancel: CancellationToken) -> Result<String> {
        ensure!(!cancel.is_cancelled(), "语音转写已取消");
        if recording
            .samples
            .iter()
            .all(|sample| *sample == 0.)
        {
            return Ok(String::new());
        }
        let mut state = self.context.create_state()?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(None);
        params.set_translate(false);
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(
            std::thread::available_parallelism().map_or(1, |count| count.get().min(4)) as i32,
        );
        unsafe extern "C" fn abort(user_data: *mut std::ffi::c_void) -> bool {
            // SAFETY: transcribe keeps the shared token alive until synchronous inference returns.
            unsafe { &*user_data.cast::<CancellationToken>() }.is_cancelled()
        }
        // SAFETY: full invokes this callback synchronously, including on joined worker threads.
        // CancellationToken supports shared access and outlives full; no pointer is retained.
        // whisper-rs 0.16's safe wrapper casts a boxed trait object to the closure type.
        unsafe {
            params.set_abort_callback(Some(abort));
            params.set_abort_callback_user_data(
                std::ptr::from_ref(&cancel)
                    .cast_mut()
                    .cast(),
            );
        }
        state
            .full(params, &recording.samples)
            .context("本地语音转写失败")?;
        ensure!(!cancel.is_cancelled(), "语音转写已取消");
        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(segment.to_str()?);
        }
        Ok(text.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_preserves_pcm_and_rejects_overflow_without_appending() {
        let mut recording = Recording::default();
        recording
            .push(&[i16::MIN, 0, i16::MAX])
            .unwrap();
        assert_eq!(recording.samples, [-1., 0., 32_767. / 32_768.]);
        let too_long = vec![0; SAMPLE_RATE_HZ * MAX_RECORDING_SECONDS];
        assert!(recording.push(&too_long).is_err());
        assert_eq!(recording.samples.len(), 3);
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
                download(&path, &url, bytes.len() as u64, &hash, &cancel, &progress)
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
        download(&path, &url, bytes.len() as u64, &hash, &cancel, &progress)
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
        let operation = download(&path, &url, 100, "unused", &cancel, &progress);
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
    #[ignore = "requires a downloaded model and independently synthesized 16 kHz mono PCM"]
    async fn local_model_transcribes_generated_speech_without_network() {
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
        let mut recording = Recording::default();
        recording.push(&samples).unwrap();
        let result = engine
            .transcribe(recording, CancellationToken::new())
            .unwrap();
        assert!(
            result
                .to_lowercase()
                .contains("local speech"),
            "{result}"
        );
        let mut recording = Recording::default();
        recording.push(&samples).unwrap();
        let cancel = CancellationToken::new();
        std::thread::scope(|scope| {
            let interrupt = cancel.clone();
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                interrupt.cancel();
            });
            let started = std::time::Instant::now();
            assert!(
                engine
                    .transcribe(recording, cancel)
                    .is_err()
            );
            assert!(started.elapsed() < Duration::from_secs(10));
        });
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            engine
                .transcribe(Recording::default(), cancel)
                .is_err()
        );
        assert_eq!(
            engine
                .transcribe(Recording::default(), CancellationToken::new())
                .unwrap(),
            ""
        );
    }
}
