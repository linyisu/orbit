use std::sync::mpsc;

use super::*;

#[test]
fn native_model_mapping_supports_unicode_paths_and_positional_reads() {
    use qwen_asr::safetensors::SafetensorsFile;
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join("合成模型.safetensors");
    let mut header = serde_json::json!({
        "sample": { "dtype": "F32", "shape": [2], "data_offsets": [0, 8] }
    })
    .to_string();
    while !header.len().is_multiple_of(8) {
        header.push(' ');
    }
    let data = [1.25_f32, -2.5_f32]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>();
    let mut bytes = (header.len() as u64)
        .to_le_bytes()
        .to_vec();
    bytes.extend_from_slice(header.as_bytes());
    bytes.extend_from_slice(&data);
    std::fs::write(&path, &bytes).unwrap();
    {
        let model = SafetensorsFile::open(path.to_str().unwrap()).unwrap();
        let tensor = model.find("sample").unwrap();
        assert_eq!(model.get_f32(tensor).unwrap(), [1.25, -2.5]);
        let mut last = [0; 4];
        assert!(model.read_tensor_bytes(tensor, 4, &mut last));
        assert_eq!(last, (-2.5_f32).to_le_bytes());
        std::thread::scope(|scope| {
            for offset in [0, 4, 0, 4] {
                let model = &model;
                let expected = &data[offset..offset + 4];
                scope.spawn(move || {
                    for _ in 0..128 {
                        let mut actual = [0; 4];
                        assert!(model.read_tensor_bytes(tensor, offset, &mut actual));
                        assert_eq!(actual, expected);
                    }
                });
            }
        });
    }
    bytes.truncate(bytes.len() - 4);
    std::fs::write(&path, bytes).unwrap();
    assert!(SafetensorsFile::open(path.to_str().unwrap()).is_none());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn native_quantized_sidecar_roundtrips_without_posix_file_apis() {
    use qwen_asr::int8_sidecar::{SidecarLayout, SidecarMmap, write_sidecar};
    let directory = tempfile::tempdir().unwrap();
    let config = qwen_asr::config::QwenConfig {
        dec_layers: 1,
        dec_hidden: 4,
        dec_intermediate: 8,
        dec_heads: 1,
        dec_kv_heads: 1,
        dec_head_dim: 4,
        vocab_size: 8,
        ..Default::default()
    };
    let layout = SidecarLayout::compute(&config);
    let buffers: Vec<_> = layout
        .lens
        .iter()
        .map(|len| vec![0; *len as usize])
        .collect();
    let slices: Vec<_> = buffers
        .iter()
        .map(Vec::as_slice)
        .collect();
    let path = directory.path().join("合成.sidecar");
    assert!(write_sidecar(
        path.to_str().unwrap(),
        directory.path().to_str().unwrap(),
        &config,
        &layout,
        &slices
    ));
    {
        let mapped = SidecarMmap::open_valid(
            path.to_str().unwrap(),
            directory.path().to_str().unwrap(),
            &config,
            &layout,
        )
        .unwrap();
        assert_eq!(mapped.i8_buf(0).len(), buffers[0].len());
        assert!(
            mapped
                .i8_buf(0)
                .iter()
                .all(|&value| value == 0)
        );
    }
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
#[ignore = "requires verified Qwen weights and independent continuous speech longer than two minutes"]
async fn local_model_keeps_recording_with_native_callback_cadence_beyond_two_minutes() {
    use std::time::{Duration, Instant};

    let directory = PathBuf::from(std::env::var_os("ORBIT_SPEECH_TEST_MODEL_DIRECTORY").unwrap());
    let path = std::env::var("ORBIT_SPEECH_LONG_TEST_WAV").unwrap();
    let samples: Vec<_> = qwen_asr::audio::load_wav(&path)
        .unwrap()
        .into_iter()
        .map(|sample| (sample.clamp(-1., 1.) * 32_767.) as i16)
        .collect();
    assert!(samples.len() > SAMPLE_RATE_HZ * 120);
    let (progress, _) = watch::channel(Progress::default());
    let engine = prepare(directory, CancellationToken::new(), progress)
        .await
        .unwrap();
    let (updates, partials) = watch::channel(String::new());
    let (mut capture, recognition) = engine.start(CancellationToken::new(), updates);
    let worker = tokio::task::spawn_blocking(move || recognition.run());
    let started = Instant::now();
    let mut first_text = None;
    let mut slowest_push = Duration::ZERO;
    for (index, chunk) in samples
        .chunks(SAMPLE_RATE_HZ / 100)
        .enumerate()
    {
        let deadline = started + Duration::from_millis((index as u64 + 1) * 10);
        tokio::time::sleep(deadline.saturating_duration_since(Instant::now())).await;
        let pushing = Instant::now();
        capture.push(chunk).unwrap();
        slowest_push = slowest_push.max(pushing.elapsed());
        if !partials.borrow().is_empty() {
            first_text.get_or_insert_with(|| started.elapsed());
        }
    }
    assert!(
        first_text.is_some(),
        "no preview while the microphone was open"
    );
    drop(capture);
    let finishing = Instant::now();
    let text = tokio::time::timeout(Duration::from_secs(60), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!text.is_empty());
    assert!(!text.contains('�'));
    assert_eq!(*partials.borrow(), text);
    eprintln!(
        "QWEN_NATIVE_CALLBACKS={}",
        serde_json::json!({
            "audio_seconds": samples.len() as f64 / SAMPLE_RATE_HZ as f64,
            "first_text_seconds": first_text.unwrap().as_secs_f64(),
            "finish_seconds": finishing.elapsed().as_secs_f64(),
            "slowest_capture_seconds": slowest_push.as_secs_f64(),
            "text": text,
        })
    );
}

#[tokio::test]
#[ignore = "requires verified local models and independently synthesized Mandarin PCM"]
async fn local_model_streams_and_corrects_before_input_finishes() {
    let directory = PathBuf::from(std::env::var_os("ORBIT_SPEECH_TEST_MODEL_DIRECTORY").unwrap());
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
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let before = partials.borrow_and_update().clone();
    assert!(!before.is_empty(), "no live text before microphone stopped");
    // Keep the microphone open through silence: Qwen must correct at a sentence boundary,
    // rather than waiting until the user stops the recording.
    for _ in 0..60 {
        capture
            .push(&vec![0; AUDIO_CHUNK_SAMPLES])
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let corrected = partials.borrow_and_update().clone();
    assert!(
        corrected.contains('。'),
        "no Qwen sentence correction while recording: {corrected}"
    );
    for chunk in samples.chunks(AUDIO_CHUNK_SAMPLES) {
        capture.push(chunk).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    drop(capture);
    let text = worker.await.unwrap().unwrap();
    assert!(
        text.starts_with(&corrected),
        "a later utterance replaced committed text"
    );
    eprintln!("Live: {before}; corrected: {text}");
    let expected: Vec<_> = "你好现在开始测试今天下午三点开会请把报告保存在项目目录"
        .repeat(2)
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
    assert!(distance[expected.len()] * 10 <= expected.len(), "{text}");
    let (updates, _) = watch::channel(String::new());
    let (mut capture, recognition) = engine.start(CancellationToken::new(), updates);
    capture
        .push(&samples[..SAMPLE_RATE_HZ * 2])
        .unwrap();
    drop(capture);
    let boundary = tokio::task::spawn_blocking(move || recognition.run())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !boundary.is_empty(),
        "exact chunk boundary discarded all provisional tokens"
    );
    assert!(!boundary.contains('�'));
    let (updates, _) = watch::channel(String::new());
    let (mut capture, recognition) = engine.start(CancellationToken::new(), updates);
    capture
        .push(&vec![0; SAMPLE_RATE_HZ])
        .unwrap();
    drop(capture);
    assert!(
        tokio::task::spawn_blocking(move || recognition.run())
            .await
            .unwrap()
            .unwrap()
            .is_empty()
    );
    let cancel = CancellationToken::new();
    let (updates, _) = watch::channel(String::new());
    let (_capture, recognition) = engine.start(cancel.clone(), updates);
    let worker = tokio::task::spawn_blocking(move || recognition.run());
    cancel.cancel();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires verified local Qwen weights"]
async fn cancellation_discards_native_loading_without_blocking_the_caller() {
    let directory = PathBuf::from(std::env::var_os("ORBIT_SPEECH_TEST_MODEL_DIRECTORY").unwrap());
    let (progress, mut updates) = watch::channel(Progress::Waiting);
    let cancel = CancellationToken::new();
    let loading = tokio::spawn(prepare(directory.clone(), cancel.clone(), progress));
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            if *updates.borrow_and_update() == Progress::LoadingModel {
                break;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(250), loading)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    // A retry waits for the cancelled native call's cache guard, then loads its own result.
    let (progress, _) = watch::channel(Progress::Waiting);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(90),
            prepare(directory, CancellationToken::new(), progress)
        )
        .await
        .unwrap()
        .is_ok()
    );
}

#[test]
fn cancellation_wakes_a_native_waiter_while_another_operation_still_owns_the_pool() {
    let owner = CancellationToken::new();
    let _guard = model::inference_guard(&owner).unwrap();
    let cancel = CancellationToken::new();
    let waiting = cancel.clone();
    let (ready, started) = mpsc::channel();
    let (result, completed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready.send(()).unwrap();
        result
            .send(model::inference_guard(&waiting).is_err())
            .unwrap();
    });
    started
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    cancel.cancel();
    assert!(
        completed
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
    );
    worker.join().unwrap();
}
#[test]
#[ignore = "subprocess fixture for isolated native cache environment"]
fn isolated_native_model_cache() {
    let Some(root) = std::env::var_os("ORBIT_SPEECH_CACHE_TEST_ROOT").map(std::path::PathBuf::from)
    else {
        return;
    };
    let expected = if cfg!(windows) {
        root.join("local/orbit/speech")
    } else {
        root.join("home/.cache/orbit/speech")
    };
    assert_eq!(super::model_directory().unwrap(), expected);
}

#[test]
fn native_model_cache_works_without_unix_environment_on_windows() {
    let root = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "speech::tests::isolated_native_model_cache",
        ])
        .env("ORBIT_SPEECH_CACHE_TEST_ROOT", root.path())
        .env_remove("XDG_CACHE_HOME")
        .env("USERPROFILE", root.path().join("home"))
        .env("LOCALAPPDATA", root.path().join("local"));
    if cfg!(windows) {
        command.env_remove("HOME");
    } else {
        command.env("HOME", root.path().join("home"));
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
