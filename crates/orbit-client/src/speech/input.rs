//! Sample-counted microphone backlog; packet frequency does not determine capacity.
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use anyhow::{Result, ensure};

use super::{AUDIO_CHUNK_SAMPLES, CancellationToken, Capture, SAMPLE_RATE_HZ};

const MAX_BACKLOG_SAMPLES: usize = SAMPLE_RATE_HZ * 30;
const INPUT_POLL: Duration = Duration::from_millis(100);

#[derive(Default)]
enum Status {
    #[default]
    Recording,
    Finished,
    Stopped,
}

#[derive(Default)]
struct Queued {
    samples: VecDeque<i16>,
    status: Status,
}

#[derive(Default)]
pub(super) struct Input {
    queued: Mutex<Queued>,
    ready: Condvar,
}

impl Input {
    pub(super) fn start() -> (Capture, Arc<Self>) {
        let input = Arc::new(Self::default());
        (
            Capture {
                input: input.clone(),
            },
            input,
        )
    }

    pub(super) fn receive(&self, cancel: &CancellationToken) -> Result<Option<Vec<f32>>> {
        let mut queued = self
            .queued
            .lock()
            .map_err(|_| anyhow::anyhow!("语音采集队列不可用"))?;
        loop {
            ensure!(!cancel.is_cancelled(), "语音转写已取消");
            if queued.samples.len() >= AUDIO_CHUNK_SAMPLES
                || !matches!(queued.status, Status::Recording)
            {
                let count = queued
                    .samples
                    .len()
                    .min(AUDIO_CHUNK_SAMPLES);
                return Ok((count > 0).then(|| {
                    queued
                        .samples
                        .drain(..count)
                        .map(|sample| sample as f32 / 32_768.)
                        .collect()
                }));
            }
            queued = self
                .ready
                .wait_timeout(queued, INPUT_POLL)
                .map_err(|_| anyhow::anyhow!("语音采集队列不可用"))?
                .0;
        }
    }

    pub(super) fn stop(&self) {
        let mut queued = self
            .queued
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        queued.status = Status::Stopped;
        queued.samples.clear();
        self.ready.notify_all();
    }
}

impl Capture {
    /// Copy PCM without inference or I/O. Capacity counts samples, including short callbacks.
    /// An overload rejects the entire callback; accepted samples are never silently dropped.
    pub fn push(&mut self, samples: &[i16]) -> Result<()> {
        let mut queued = self
            .input
            .queued
            .lock()
            .map_err(|_| anyhow::anyhow!("语音采集队列不可用"))?;
        ensure!(
            matches!(queued.status, Status::Recording),
            "本地语音识别已停止"
        );
        ensure!(
            samples.len() <= MAX_BACKLOG_SAMPLES - queued.samples.len(),
            "本地语音识别持续落后，待处理音频已超过 30 秒；已保留当前文字，请稍后重试"
        );
        queued.samples.extend(samples);
        if queued.samples.len() >= AUDIO_CHUNK_SAMPLES {
            self.input.ready.notify_one();
        }
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let mut queued = self
            .input
            .queued
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(queued.status, Status::Recording) {
            queued.status = Status::Finished;
        }
        self.input.ready.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_callbacks_survive_a_normal_decode_pause() {
        let (mut capture, input) = Input::start();
        for _ in 0..150 {
            capture
                .push(&[1; SAMPLE_RATE_HZ / 100])
                .unwrap();
        }
        drop(capture);
        let cancel = CancellationToken::new();
        let mut received = Vec::new();
        while let Some(samples) = input.receive(&cancel).unwrap() {
            received.extend(samples);
        }
        assert_eq!(received, vec![1. / 32_768.; SAMPLE_RATE_HZ * 3 / 2]);
    }

    #[test]
    fn backlog_limit_is_independent_of_packet_sizes_and_rejects_atomically() {
        for packet in [1, 160, 1_600, 16_000] {
            let (mut capture, input) = Input::start();
            for _ in 0..MAX_BACKLOG_SAMPLES / packet {
                capture.push(&vec![1; packet]).unwrap();
            }
            assert!(capture.push(&[2; 3]).is_err());
            assert_eq!(
                input
                    .queued
                    .lock()
                    .unwrap()
                    .samples
                    .len(),
                MAX_BACKLOG_SAMPLES
            );
            input
                .receive(&CancellationToken::new())
                .unwrap();
            capture
                .push(&[3; AUDIO_CHUNK_SAMPLES])
                .unwrap();
        }
    }

    #[test]
    fn finish_flushes_a_short_tail_once_and_preserves_pcm() {
        let (mut capture, input) = Input::start();
        capture
            .push(&[i16::MIN, 0, i16::MAX])
            .unwrap();
        drop(capture);
        let cancel = CancellationToken::new();
        assert_eq!(
            input.receive(&cancel).unwrap().unwrap(),
            [-1., 0., 32_767. / 32_768.]
        );
        assert!(
            input
                .receive(&cancel)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn capture_can_continue_beyond_two_minutes_without_accumulating_history() {
        let (mut capture, input) = Input::start();
        let cancel = CancellationToken::new();
        for index in 0..1_500 {
            capture
                .push(&vec![index; AUDIO_CHUNK_SAMPLES])
                .unwrap();
            assert_eq!(
                input.receive(&cancel).unwrap().unwrap(),
                vec![index as f32 / 32_768.; AUDIO_CHUNK_SAMPLES]
            );
        }
        assert!(
            input
                .queued
                .lock()
                .unwrap()
                .samples
                .is_empty()
        );
    }

    #[test]
    fn worker_exit_rejects_late_input_and_cancellation_wakes_idle_receive() {
        let (mut capture, input) = Input::start();
        let cancel = CancellationToken::new();
        let worker_input = input.clone();
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || worker_input.receive(&worker_cancel));
        cancel.cancel();
        assert!(worker.join().unwrap().is_err());
        input.stop();
        assert!(capture.push(&[1]).is_err());
        drop(capture);
        assert!(
            input
                .receive(&CancellationToken::new())
                .unwrap()
                .is_none()
        );
    }
}
