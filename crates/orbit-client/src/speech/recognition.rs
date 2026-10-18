//! Incremental Qwen audio windows and KV reuse; capture and all native work stay separate.
use std::{sync::mpsc, time::Duration};

use anyhow::{Context, Result, ensure};
use qwen_asr::{context::QwenCtx, transcribe::StreamState};

use super::{
    CancellationToken, Recognition, Recognizer, SAMPLE_RATE_HZ, model::inference_guard,
    transcript::Transcript,
};

const INPUT_POLL: Duration = Duration::from_millis(100);
const DECODE_CHUNK_SAMPLES: usize = SAMPLE_RATE_HZ * 2;
const PRE_ROLL_SAMPLES: usize = SAMPLE_RATE_HZ / 5;
const TRAILING_SILENCE_SAMPLES: usize = SAMPLE_RATE_HZ * 3 / 5;
const SPEECH_RMS_FLOOR: f64 = 0.002;

#[derive(Default)]
enum Activity {
    #[default]
    Waiting,
    Speaking {
        quiet_samples: usize,
    },
}

/// The buffer is a short pre-roll while waiting, and a bounded utterance while speaking.
#[derive(Default)]
struct Audio {
    samples: Vec<f32>,
    activity: Activity,
}

impl Audio {
    fn push(&mut self, samples: &[f32]) -> bool {
        let speech = !samples.is_empty()
            && samples
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                > SPEECH_RMS_FLOOR.powi(2) * samples.len() as f64;
        self.samples.extend_from_slice(samples);
        match &mut self.activity {
            Activity::Waiting if speech => {
                self.activity = Activity::Speaking { quiet_samples: 0 };
                false
            }
            Activity::Waiting => {
                if self.samples.len() > PRE_ROLL_SAMPLES {
                    self.samples
                        .drain(..self.samples.len() - PRE_ROLL_SAMPLES);
                }
                false
            }
            Activity::Speaking { quiet_samples } => {
                if speech {
                    *quiet_samples = 0;
                } else {
                    *quiet_samples += samples.len();
                }
                *quiet_samples >= TRAILING_SILENCE_SAMPLES
            }
        }
    }

    fn is_speaking(&self) -> bool {
        matches!(self.activity, Activity::Speaking { .. })
    }

    fn reset(&mut self) {
        self.samples.clear();
        self.activity = Activity::Waiting;
    }
}

struct StreamingSession {
    decoder: QwenCtx,
    state: StreamState,
    audio: Audio,
    transcript: Transcript,
}

impl StreamingSession {
    fn new(engine: &Recognizer, cancel: &CancellationToken) -> Result<Self> {
        const ENCODER_WINDOW_FRAMES: usize = 200;
        const ROLLBACK_TOKENS: i32 = 12;
        const UNFIXED_CHUNKS: i32 = 2;
        const MAX_NEW_TOKENS: i32 = 64;
        let _guard = inference_guard(cancel)?;
        let mut decoder = engine.model.new_session();
        decoder.config.enc_n_window_infer = ENCODER_WINDOW_FRAMES;
        decoder.stream_chunk_sec = DECODE_CHUNK_SAMPLES as f32 / SAMPLE_RATE_HZ as f32;
        decoder.stream_rollback = ROLLBACK_TOKENS;
        decoder.stream_unfixed_chunks = UNFIXED_CHUNKS;
        decoder.stream_max_new_tokens = MAX_NEW_TOKENS;
        decoder.past_text_conditioning = true;
        decoder
            .set_force_language("Chinese")
            .map_err(|()| anyhow::anyhow!("无法配置 Qwen 中英文识别"))?;
        Ok(Self {
            decoder,
            state: StreamState::new(),
            audio: Audio::default(),
            transcript: Transcript::default(),
        })
    }

    fn push(&mut self, samples: &[f32], cancel: &CancellationToken) -> Result<()> {
        let end = self.audio.push(samples);
        if self.audio.is_speaking() {
            if end {
                self.finish(cancel)?;
            } else {
                while self.audio.samples.len() - self.state.audio_cursor() >= DECODE_CHUNK_SAMPLES {
                    let end = self.state.audio_cursor() + DECODE_CHUNK_SAMPLES;
                    self.decode(end, false, cancel)?;
                }
            }
        }
        Ok(())
    }

    fn decode(&mut self, end: usize, finalize: bool, cancel: &CancellationToken) -> Result<()> {
        let _guard = inference_guard(cancel)?;
        qwen_asr::transcribe::stream_push_audio(
            &mut self.decoder,
            &self.audio.samples[..end],
            &mut self.state,
            finalize,
        )
        .context("Qwen 增量识别未完成，已保留先前文字")?;
        ensure!(!cancel.is_cancelled(), "语音转写已取消");
        self.transcript
            .preview(self.state.hypothesis());
        Ok(())
    }

    fn finish(&mut self, cancel: &CancellationToken) -> Result<()> {
        if self.audio.is_speaking() {
            self.decode(self.audio.samples.len(), true, cancel)?;
            self.transcript.commit();
            self.state.reset();
            self.audio.reset();
        }
        Ok(())
    }
}

impl Recognition {
    /// Produce the first hypothesis and subsequent revisions with the same Qwen stream.
    /// Completed encoder windows and matching decoder prefixes are reused. Stopping flushes
    /// the remaining window and rollback tail; cancellation prevents further publication.
    pub fn run(self) -> Result<String> {
        let mut session = StreamingSession::new(&self.engine, &self.cancel)?;
        loop {
            ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
            match self.receiver.recv_timeout(INPUT_POLL) {
                Ok(samples) => session.push(&samples, &self.cancel)?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.publish(session.transcript.text())?;
        }
        session.finish(&self.cancel)?;
        let text = session.transcript.text();
        self.publish(text.clone())?;
        Ok(text)
    }

    fn publish(&self, text: String) -> Result<()> {
        ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
        self.updates
            .send_if_modified(|previous| {
                if *previous == text {
                    false
                } else {
                    *previous = text;
                    true
                }
            });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_keeps_only_preroll_and_never_starts_inference() {
        let mut audio = Audio::default();
        for _ in 0..100 {
            assert!(!audio.push(&[0.; 1_600]));
        }
        assert!(!audio.is_speaking());
        assert_eq!(audio.samples.len(), PRE_ROLL_SAMPLES);
        assert!(!audio.push(&[0.05; 1_600]));
        assert!(audio.is_speaking());
        for _ in 0..5 {
            assert!(!audio.push(&[0.; 1_600]));
        }
        assert!(audio.push(&[0.; 1_600]));
        audio.reset();
        assert!(!audio.is_speaking());
        assert!(audio.samples.is_empty());
    }

    #[test]
    fn speech_resets_the_silence_boundary() {
        let mut audio = Audio::default();
        audio.push(&[0.05; 1_600]);
        audio.push(&[0.; 8_000]);
        assert!(!audio.push(&[0.05; 1_600]));
        assert!(!audio.push(&[0.; 8_000]));
        assert!(audio.push(&[0.; 1_600]));
    }

    #[test]
    #[ignore = "requires verified Qwen weights and independent benchmark WAV manifest"]
    fn benchmark_incremental_qwen_corpus() {
        use std::{path::PathBuf, time::Instant};
        let root = PathBuf::from(std::env::var_os("ORBIT_SPEECH_TEST_MODEL_DIRECTORY").unwrap());
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::env::var_os("ORBIT_SPEECH_BENCHMARK_MANIFEST").unwrap()).unwrap(),
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let (progress, _) = tokio::sync::watch::channel(super::super::Progress::Waiting);
        let loading = Instant::now();
        let engine = super::super::model::load(&root, &cancel, &progress).unwrap();
        eprintln!("QWEN_LOAD_SECONDS={:.3}", loading.elapsed().as_secs_f64());
        for item in manifest.as_array().unwrap() {
            let samples = qwen_asr::audio::load_wav(item["path"].as_str().unwrap()).unwrap();
            let mut session = StreamingSession::new(&engine, &cancel).unwrap();
            if let Ok(rollback) = std::env::var("ORBIT_SPEECH_BENCHMARK_ROLLBACK") {
                session.decoder.stream_rollback = rollback.parse().unwrap();
            }
            let started = Instant::now();
            let mut first = None;
            let mut compute = Duration::ZERO;
            let mut previous = String::new();
            let mut revisions = 0;
            let mut slowest = Duration::ZERO;
            let mut max_cached_windows = 0;
            for (index, chunk) in samples.chunks(1_600).enumerate() {
                let deadline = started + Duration::from_millis((index as u64 + 1) * 100);
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                let processing = Instant::now();
                session.push(chunk, &cancel).unwrap();
                max_cached_windows = max_cached_windows.max(session.state.enc_window_count());
                assert!(
                    max_cached_windows <= 7,
                    "encoder context grew beyond fourteen seconds"
                );
                let duration = processing.elapsed();
                compute += duration;
                slowest = slowest.max(duration);
                let text = session.transcript.text();
                if !text.is_empty() {
                    first.get_or_insert_with(|| started.elapsed().as_secs_f64());
                }
                if !previous.is_empty() && !text.starts_with(&previous) {
                    revisions += 1;
                }
                previous = text;
            }
            let finishing = Instant::now();
            session.finish(&cancel).unwrap();
            let finish = finishing.elapsed();
            compute += finish;
            let work = session.state.work();
            assert!(
                work.encoder_samples <= samples.len(),
                "encoder replayed an audio prefix"
            );
            assert!(
                work.reused_prefill_rows > 0,
                "decoder did not reuse KV rows"
            );
            eprintln!(
                "QWEN_BENCHMARK={}",
                serde_json::json!({
                    "id": item["id"],
                    "condition": item["condition"],
                    "text": session.transcript.text(),
                    "audio_seconds": samples.len() as f64 / SAMPLE_RATE_HZ as f64,
                    "first_text_seconds": first,
                    "compute_seconds": compute.as_secs_f64(),
                    "finish_seconds": finish.as_secs_f64(),
                    "slowest_push_seconds": slowest.as_secs_f64(),
                    "revisions": revisions,
                    "encoder_samples": work.encoder_samples,
                    "encoder_windows": work.encoder_windows,
                    "max_cached_windows": max_cached_windows,
                    "reused_prefill_rows": work.reused_prefill_rows,
                    "prefill_rows": work.prefill_rows,
                })
            );
        }
    }
}
