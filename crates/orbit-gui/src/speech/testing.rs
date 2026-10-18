//! Deterministic microphone replacement for isolated native acceptance.
use super::*;
use gpui_kit::component::speech::{AudioFormat, AudioInput, AudioSink};

pub(super) struct FileInput(pub std::path::PathBuf);

impl AudioInput for FileInput {
    fn start(
        &self,
        format: AudioFormat,
        sink: AudioSink,
        cx: &mut App,
    ) -> Result<Subscription, SpeechError> {
        let samples = (|| -> anyhow::Result<Vec<i16>> {
            anyhow::ensure!(
                format.sample_rate() == speech::SAMPLE_RATE_HZ as u32 && format.channels() == 1,
                "test audio requires 16 kHz mono"
            );
            anyhow::ensure!(
                std::fs::metadata(&self.0)?.len()
                    <= (speech::SAMPLE_RATE_HZ * speech::MAX_RECORDING_SECONDS * 2) as u64,
                "test audio is too large"
            );
            let bytes = std::fs::read(&self.0)?;
            anyhow::ensure!(
                bytes.len().is_multiple_of(2),
                "test audio has an incomplete sample"
            );
            Ok(bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|sample| i16::from_le_bytes(*sample))
                .collect())
        })()
        .map_err(SpeechError::input)?;
        let task = cx.spawn(async move |cx| {
            let count = speech::SAMPLE_RATE_HZ / 10;
            for chunk in samples.chunks(count) {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                cx.update(|cx| sink.push(chunk.to_vec(), cx));
            }
        });
        Ok(Subscription::new(move || drop(task)))
    }
}
