//! Client-host Qwen incremental speech recognition and background model preparation.
//! Only verified model downloads use the network; audio stays on this client host.
mod cache;
mod input;
mod model;
mod recognition;
mod transcript;

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use tokio::sync::watch;
pub use tokio_util::sync::CancellationToken;

pub const SAMPLE_RATE_HZ: usize = 16_000;
const AUDIO_CHUNK_SAMPLES: usize = 1_600;

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
    LoadingModel,
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

/// Download or resume verified assets and load Qwen off the caller's executor.
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

/// Native Qwen weights shared across windows; each recording owns its incremental caches.
pub struct Recognizer {
    model: Arc<qwen_asr::context::QwenModel>,
}

/// Bounded microphone input. Dropping it ends the input; cancellation discards all hypotheses.
pub struct Capture {
    input: Arc<input::Input>,
}

/// One incremental recording session. Run it on a worker; updates replace the full voice span.
pub struct Recognition {
    engine: Arc<Recognizer>,
    input: Arc<input::Input>,
    cancel: CancellationToken,
    updates: watch::Sender<String>,
}

impl Recognizer {
    /// Start a session without performing inference on the caller thread.
    /// PCM backlog is bounded by sample count; recording has no fixed duration limit.
    pub fn start(
        self: &Arc<Self>,
        cancel: CancellationToken,
        updates: watch::Sender<String>,
    ) -> (Capture, Recognition) {
        let (capture, input) = input::Input::start();
        (
            capture,
            Recognition {
                engine: self.clone(),
                input,
                cancel,
                updates,
            },
        )
    }
}

#[cfg(test)]
mod tests;

impl Drop for Recognition {
    fn drop(&mut self) {
        self.input.stop();
    }
}
