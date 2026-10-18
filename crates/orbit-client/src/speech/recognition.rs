//! Live decoding and a separate serialized Qwen worker; audio capture never waits for correction.
use std::{
    sync::{
        Arc, Mutex, TryLockError,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use sherpa_onnx::{OfflineRecognizer, OnlineStream};

use super::{CancellationToken, Recognition, Recognizer, SAMPLE_RATE_HZ, transcript::Transcript};

const INPUT_POLL: Duration = Duration::from_millis(100);
const CORRECTION_LOCK_POLL: Duration = Duration::from_millis(25);

struct SentenceAudio {
    index: usize,
    samples: Vec<f32>,
}

type Correction = Result<(usize, String)>;

impl Recognition {
    /// Stream live hypotheses while Qwen corrects completed sentences on another worker.
    /// A correction failure returns an error without clearing the published preview.
    pub fn run(self) -> Result<String> {
        let cancel = self.cancel.child_token();
        let engine = self.engine.clone();
        let correction_cancel = cancel.clone();
        // The unconsumed audio in these jobs is bounded by Capture's 120-second session cap.
        // Replaying prefixes is deliberately avoided: each completed sentence is decoded once.
        let (jobs, receiver) = mpsc::channel();
        let (corrected, results) = mpsc::channel();
        std::thread::scope(|scope| {
            let correction = scope
                .spawn(move || correct_sentences(&engine, receiver, corrected, &correction_cancel));
            let result = self.decode(&jobs, &results);
            cancel.cancel();
            drop(jobs);
            let joined = correction.join();
            let text = result?;
            ensure!(joined.is_ok(), "句尾校正线程意外停止，已保留实时预览");
            Ok(text)
        })
    }

    fn decode(
        &self,
        jobs: &Sender<SentenceAudio>,
        results: &Receiver<Correction>,
    ) -> Result<String> {
        let stream = self.engine.preview.create_stream();
        let mut transcript = Transcript::default();
        let mut audio = Vec::new();
        loop {
            ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
            match self.receiver.recv_timeout(INPUT_POLL) {
                Ok(samples) => {
                    stream.accept_waveform(SAMPLE_RATE_HZ as i32, &samples);
                    audio.extend_from_slice(&samples);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    apply_corrections(results, &mut transcript)?;
                    self.publish(&transcript)?;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.decode_preview(&stream, &mut transcript)?;
            if self.engine.preview.is_endpoint(&stream) {
                queue_sentence(jobs, &mut transcript, &mut audio)?;
                self.engine.preview.reset(&stream);
            }
            apply_corrections(results, &mut transcript)?;
            self.publish(&transcript)?;
        }
        ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
        stream.accept_waveform(SAMPLE_RATE_HZ as i32, &vec![0.; SAMPLE_RATE_HZ * 4 / 5]);
        stream.input_finished();
        self.decode_preview(&stream, &mut transcript)?;
        queue_sentence(jobs, &mut transcript, &mut audio)?;
        self.publish(&transcript)?;
        while transcript.pending() {
            ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
            match results.recv_timeout(INPUT_POLL) {
                Ok(result) => {
                    let (index, text) = result?;
                    transcript.correct(index, text)?;
                    self.publish(&transcript)?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("句尾校正未完成，已保留实时预览");
                }
            }
        }
        ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
        Ok(transcript.text())
    }

    fn decode_preview(&self, stream: &OnlineStream, transcript: &mut Transcript) -> Result<()> {
        while self.engine.preview.is_ready(stream) {
            ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
            self.engine.preview.decode(stream);
        }
        transcript.preview(
            self.engine
                .preview
                .get_result(stream)
                .context("无法读取实时语音结果")?
                .text,
        );
        Ok(())
    }

    fn publish(&self, transcript: &Transcript) -> Result<()> {
        ensure!(!self.cancel.is_cancelled(), "语音转写已取消");
        let text = transcript.text();
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

fn queue_sentence(
    jobs: &Sender<SentenceAudio>,
    transcript: &mut Transcript,
    samples: &mut Vec<f32>,
) -> Result<()> {
    if let Some(index) = transcript.finish_sentence() {
        jobs.send(SentenceAudio {
            index,
            samples: std::mem::take(samples),
        })
        .context("句尾校正服务已停止，已保留实时预览")?;
    } else {
        samples.clear();
    }
    Ok(())
}

fn apply_corrections(results: &Receiver<Correction>, transcript: &mut Transcript) -> Result<()> {
    loop {
        match results.try_recv() {
            Ok(result) => {
                let (index, text) = result?;
                transcript.correct(index, text)?;
            }
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                anyhow::bail!("句尾校正服务已停止，已保留实时预览");
            }
        }
    }
}

fn correct_sentences(
    engine: &Arc<Recognizer>,
    jobs: Receiver<SentenceAudio>,
    results: Sender<Correction>,
    cancel: &CancellationToken,
) {
    while !cancel.is_cancelled() {
        let job = match jobs.recv_timeout(INPUT_POLL) {
            Ok(job) => job,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let result =
            correct(&engine.correction, &job.samples, cancel).map(|text| (job.index, text));
        if cancel.is_cancelled() {
            return;
        }
        let failed = result.is_err();
        if results.send(result).is_err() || failed {
            return;
        }
    }
}

fn correct(
    engine: &Mutex<OfflineRecognizer>,
    samples: &[f32],
    cancel: &CancellationToken,
) -> Result<String> {
    let engine = loop {
        ensure!(!cancel.is_cancelled(), "句尾校正已取消");
        match engine.try_lock() {
            Ok(engine) => break engine,
            Err(TryLockError::WouldBlock) => std::thread::sleep(CORRECTION_LOCK_POLL),
            Err(TryLockError::Poisoned(_)) => anyhow::bail!("句尾校正服务异常，请重新启动语音输入"),
        }
    };
    let stream = engine.create_stream();
    stream.accept_waveform(SAMPLE_RATE_HZ as i32, samples);
    engine.decode(&stream);
    ensure!(!cancel.is_cancelled(), "句尾校正已取消");
    let text = stream
        .get_result()
        .context("无法读取 Qwen 校正结果")?
        .text;
    ensure!(
        !text.trim().is_empty(),
        "Qwen 未返回校正文字，已保留实时预览"
    );
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_errors_and_disconnection_keep_completed_preview() {
        let mut transcript = Transcript::default();
        transcript.preview("独立生成的预览".into());
        transcript.finish_sentence();
        transcript.preview("继续输入".into());
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Err(anyhow::anyhow!("generated correction failure")))
            .unwrap();
        assert!(apply_corrections(&receiver, &mut transcript).is_err());
        assert_eq!(transcript.text(), "独立生成的预览继续输入");
        drop(sender);
        assert!(apply_corrections(&receiver, &mut transcript).is_err());
        assert_eq!(transcript.text(), "独立生成的预览继续输入");
    }

    #[test]
    fn silence_is_not_submitted_to_the_generative_corrector() {
        let (sender, receiver) = mpsc::channel();
        let mut transcript = Transcript::default();
        let mut audio = vec![0.; 16_000];
        queue_sentence(&sender, &mut transcript, &mut audio).unwrap();
        assert!(audio.is_empty());
        assert!(receiver.try_recv().is_err());
        transcript.preview("测试".into());
        audio.extend_from_slice(&[0.1, 0.2]);
        queue_sentence(&sender, &mut transcript, &mut audio).unwrap();
        assert!(audio.is_empty());
        assert_eq!(receiver.recv().unwrap().samples, [0.1, 0.2]);
        assert_eq!(transcript.text(), "测试");
    }
}
