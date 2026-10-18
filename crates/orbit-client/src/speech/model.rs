//! Verified Qwen weights and serialized access to its process-wide native kernel pool.
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use qwen_asr::context::QwenModel;
use tokio::sync::watch;

use super::{CancellationToken, Progress, Recognizer, cache::Asset};

const MODEL_SOURCE: &str = "https://huggingface.co/Qwen/Qwen3-ASR-0.6B/resolve/\
    5eb144179a02acc5e5ba31e748d22b0cf3e303b0";
const INFERENCE_LOCK_POLL: Duration = Duration::from_millis(25);
static INFERENCE: Mutex<()> = Mutex::new(());

pub(super) const ASSETS: [Asset<'static>; 2] = [
    Asset {
        directory: "qwen3-asr-0.6b",
        name: "model.safetensors",
        source: MODEL_SOURCE,
        bytes: 1_876_091_704,
        sha256: "79d6cbd4c98c7bbffe9db2edac07f56cd6637d0d5944b27f6c2b8353840323ea",
    },
    Asset {
        directory: "qwen3-asr-0.6b",
        name: "vocab.json",
        source: MODEL_SOURCE,
        bytes: 2_776_833,
        sha256: "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
    },
];

/// The native kernel pool publishes one dispatch at a time across all loaded models.
/// Lock only on background workers; cancelled sessions never wait for another recording.
pub(super) fn inference_guard(cancel: &CancellationToken) -> Result<MutexGuard<'static, ()>> {
    loop {
        ensure!(!cancel.is_cancelled(), "语音处理已取消");
        match INFERENCE.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::WouldBlock) => std::thread::sleep(INFERENCE_LOCK_POLL),
            Err(TryLockError::Poisoned(_)) => {
                anyhow::bail!("本地语音引擎异常，请重新启动应用");
            }
        }
    }
}

pub(super) fn load(
    root: &Path,
    cancel: &CancellationToken,
    progress: &watch::Sender<Progress>,
) -> Result<Arc<Recognizer>> {
    const KERNEL_THREADS: usize = 2;
    #[cfg(target_arch = "x86_64")]
    ensure!(
        std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma"),
        "本机 CPU 不支持本地 Qwen 识别所需的 AVX2 和 FMA 指令"
    );
    let _guard = inference_guard(cancel)?;
    progress.send_replace(Progress::LoadingModel);
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn openblas_set_num_threads(count: libc::c_int);
        }
        // SAFETY: OpenBLAS accepts a positive thread count; the inference gate excludes
        // active model operations while changing its process-wide worker configuration.
        unsafe { openblas_set_num_threads(1) };
    }
    qwen_asr::kernels::set_threads(KERNEL_THREADS);
    let root = root.join("qwen3-asr-0.6b");
    let model = QwenModel::load(
        root.to_str()
            .context("模型路径不是 UTF-8")?,
    )
    .context("无法加载 Qwen3-ASR 0.6B 增量语音模型")?;
    ensure!(!cancel.is_cancelled(), "语音准备已取消");
    Ok(Arc::new(Recognizer { model }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_interrupts_waiting_for_the_native_pool() {
        let owner = CancellationToken::new();
        let _guard = inference_guard(&owner).unwrap();
        let cancel = CancellationToken::new();
        let waiting = cancel.clone();
        let worker = std::thread::spawn(move || inference_guard(&waiting).is_err());
        cancel.cancel();
        assert!(worker.join().unwrap());
    }
}
