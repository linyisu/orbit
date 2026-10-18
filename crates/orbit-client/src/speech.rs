//! Client-host speech preparation, live previews and Qwen sentence correction.
//! Only verified model downloads use the network; audio stays on this client host.
mod cache;
mod model;
mod recognition;
mod transcript;

use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
};

use anyhow::{Context, Result, ensure};
use tokio::sync::watch;
pub use tokio_util::sync::CancellationToken;

pub const SAMPLE_RATE_HZ: usize = 16_000;
pub const MAX_RECORDING_SECONDS: usize = 120;
const AUDIO_CHUNK_SAMPLES: usize = 1_600;
const AUDIO_QUEUE_CHUNKS: usize = 32;

/// The current background preparation phase. Byte counters include cached assets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Progress {
    #[default]
    Waiting,
    Verifying {
        completed_bytes: u64,
        total_bytes: u64,
    },
    Downloading {
        received_bytes: u64,
        total_bytes: u64,
    },
    LoadingPreview,
    LoadingCorrection,
}

/// The global client-host cache, independent of local and remote projects.
pub fn model_directory() -> Result<PathBuf> {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .context("无法确定本地语音模型目录，请设置 XDG_CACHE_HOME")?;
    ensure!(root.is_absolute(), "XDG_CACHE_HOME 必须是绝对路径");
    Ok(root.join("orbit/speech"))
}

/// Download or resume verified assets and load both models off the caller's executor.
/// Cancellation preserves partial downloads and discards late loading results. A native
/// load already in progress finishes on its worker and releases resources there.
pub async fn prepare(
    directory: PathBuf,
    cancel: CancellationToken,
    progress: watch::Sender<Progress>,
) -> Result<Arc<Recognizer>> {
    let lock = cache::lock(&directory, &cancel).await?;
    cache::prepare(&directory, &model::ASSETS, &cancel, &progress).await?;
    let loading_cancel = cancel.clone();
    let loading = tokio::task::spawn_blocking(move || {
        let _lock = lock;
        model::load(&directory, &loading_cancel, &progress)
    });
    tokio::select! {
        biased;
        _ = cancel.cancelled() => anyhow::bail!("语音准备已取消"),
        result = loading => result?,
    }
}

/// Native CPU recognizers shared across windows; corrections serialize on their worker.
pub struct Recognizer {
    preview: sherpa_onnx::OnlineRecognizer,
    correction: std::sync::Mutex<sherpa_onnx::OfflineRecognizer>,
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
            self.samples += chunk.len();
        }
        Ok(())
    }
}

/// One bounded recording session. Run it on a worker; updates replace the full voice span.
pub struct Recognition {
    engine: Arc<Recognizer>,
    receiver: Receiver<Vec<f32>>,
    cancel: CancellationToken,
    updates: watch::Sender<String>,
}

impl Recognizer {
    /// Start a session without performing inference on the caller thread.
    /// PCM input retains at most 3.2 seconds of backlog; total audio is capped at 120 seconds.
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

#[cfg(test)]
mod tests;
