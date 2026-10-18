//! Pinned native model assets and their background loading configuration.
use super::{CancellationToken, Progress, Recognizer, cache::Asset};
use anyhow::{Context, Result, ensure};
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OnlineRecognizer, OnlineRecognizerConfig,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

const PREVIEW_SOURCE: &str = "https://huggingface.co/csukuangfj/\
    sherpa-onnx-streaming-paraformer-bilingual-zh-en/resolve/\
    8e40c43232a1c5c66c82111efc5820d3accca11b";
// The tokenizer and convolution frontend are identical in the FP32 and INT8 exports.
const CORRECTION_SUPPORT_SOURCE: &str = "https://huggingface.co/csukuangfj2/\
    sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/resolve/\
    68818b2313fe77bd06f6a7c5068ff3ef59d02b8a";

const CORRECTION_WEIGHTS_SOURCE: &str = "https://modelscope.cn/models/\
    zengshuishui/Qwen3-ASR-onnx/resolve/\
    de9e449eb376dcb472c1ce8141fbae8d524fabbd/model_0.6B";

pub(super) const ASSETS: [Asset<'static>; 11] = [
    Asset {
        directory: "paraformer-zh-en-int8",
        name: "encoder.int8.onnx",
        source: PREVIEW_SOURCE,
        bytes: 165_462_184,
        sha256: "81a70226a8934e6ed92aa1d4fc486b428b5398e2f2619ed4897b7294cab90e9a",
    },
    Asset {
        directory: "paraformer-zh-en-int8",
        name: "decoder.int8.onnx",
        source: PREVIEW_SOURCE,
        bytes: 71_664_561,
        sha256: "f3cca9f77bb9d93c8fcbfb63ae617b6b1ee96818df3aa3b151c40658fe38594f",
    },
    Asset {
        directory: "paraformer-zh-en-int8",
        name: "tokens.txt",
        source: PREVIEW_SOURCE,
        bytes: 75_756,
        sha256: "59aba8873a2ed1e122c25fee421e25f283b63290efbde85c1f01a853d83cb6e6",
    },
    Asset {
        directory: "qwen3-asr-0.6b-fp32",
        name: "conv_frontend.onnx",
        source: CORRECTION_SUPPORT_SOURCE,
        bytes: 44_148_281,
        sha256: "d22dc4423e0940e49884e903d2ea2f7e5567c14fc1aed97e4e26d6b8f208ef9e",
    },
    Asset {
        directory: "qwen3-asr-0.6b-fp32",
        name: "decoder.onnx",
        source: CORRECTION_WEIGHTS_SOURCE,
        bytes: 843_211,
        sha256: "cec03c39c09177b4ab33fde06e5e12d9f225214b7a0cf82ac9823f2e954f5a68",
    },
    Asset {
        directory: "qwen3-asr-0.6b-fp32",
        name: "decoder.onnx.data",
        source: CORRECTION_WEIGHTS_SOURCE,
        bytes: 3_006_500_864,
        sha256: "2d86c0640759029532d224922a4248e07dace21887dda8dff63ffd7b61d8eae0",
    },
    Asset {
        directory: "qwen3-asr-0.6b-fp32",
        name: "encoder.onnx",
        source: CORRECTION_WEIGHTS_SOURCE,
        bytes: 5_594_598,
        sha256: "710609f285ce160f55acd346648010a2daa14fa7d6703fd1d30a12a994d0d637",
    },
    Asset {
        directory: "qwen3-asr-0.6b-fp32",
        name: "encoder.onnx.data",
        source: CORRECTION_WEIGHTS_SOURCE,
        bytes: 701_367_808,
        sha256: "3082f026bf9c18b3ce1ba5408a18014b659c6dd2ff020e5ac34d8abdb50489b2",
    },
    Asset {
        directory: "qwen3-asr-0.6b-fp32",
        name: "tokenizer/merges.txt",
        source: CORRECTION_SUPPORT_SOURCE,
        bytes: 1_671_853,
        sha256: "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5",
    },
    Asset {
        directory: "qwen3-asr-0.6b-fp32",
        name: "tokenizer/tokenizer_config.json",
        source: CORRECTION_SUPPORT_SOURCE,
        bytes: 12_487,
        sha256: "4942d005604266809309cabc9f4e9cb89ce855d59b14681fdc0e1cc62ea26c4c",
    },
    Asset {
        directory: "qwen3-asr-0.6b-fp32",
        name: "tokenizer/vocab.json",
        source: CORRECTION_SUPPORT_SOURCE,
        bytes: 2_776_833,
        sha256: "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
    },
];

pub(super) fn load(
    root: &Path,
    cancel: &CancellationToken,
    progress: &watch::Sender<Progress>,
) -> Result<Arc<Recognizer>> {
    const THREADS_PER_MODEL: i32 = 2;
    const MAX_SENTENCE_SECONDS: f32 = 20.;
    const MAX_CORRECTION_TOKENS: i32 = 256;
    const CORRECTION_CONTEXT_TOKENS: i32 = 512;

    ensure!(!cancel.is_cancelled(), "语音准备已取消");
    let preview_root = root.join("paraformer-zh-en-int8");
    progress.send_replace(Progress::LoadingPreview);
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.paraformer.encoder = Some(path(&preview_root, "encoder.int8.onnx")?);
    config.model_config.paraformer.decoder = Some(path(&preview_root, "decoder.int8.onnx")?);
    config.model_config.tokens = Some(path(&preview_root, "tokens.txt")?);
    config.model_config.num_threads = THREADS_PER_MODEL;
    config.decoding_method = Some("greedy_search".into());
    config.enable_endpoint = true;
    config.rule1_min_trailing_silence = 2.4;
    config.rule2_min_trailing_silence = 1.2;
    config.rule3_min_utterance_length = MAX_SENTENCE_SECONDS;
    let preview = OnlineRecognizer::create(&config).context("无法加载实时语音模型")?;
    ensure!(!cancel.is_cancelled(), "语音准备已取消");
    progress.send_replace(Progress::LoadingCorrection);
    let root = root.join("qwen3-asr-0.6b-fp32");
    let mut config = OfflineRecognizerConfig::default();
    config
        .model_config
        .qwen3_asr
        .conv_frontend = Some(path(&root, "conv_frontend.onnx")?);
    config.model_config.qwen3_asr.encoder = Some(path(&root, "encoder.onnx")?);
    config.model_config.qwen3_asr.decoder = Some(path(&root, "decoder.onnx")?);
    config.model_config.qwen3_asr.tokenizer = Some(path(&root, "tokenizer")?);
    config
        .model_config
        .qwen3_asr
        .max_new_tokens = MAX_CORRECTION_TOKENS;
    config
        .model_config
        .qwen3_asr
        .max_total_len = CORRECTION_CONTEXT_TOKENS;
    config.model_config.tokens = Some(String::new());
    config.model_config.num_threads = THREADS_PER_MODEL;
    let correction = OfflineRecognizer::create(&config).context("无法加载 Qwen3-ASR 0.6B 模型")?;
    ensure!(!cancel.is_cancelled(), "语音准备已取消");
    Ok(Arc::new(Recognizer {
        preview,
        correction: Mutex::new(correction),
    }))
}

fn path(root: &Path, name: &str) -> Result<String> {
    Ok(root
        .join(name)
        .to_str()
        .context("模型路径不是 UTF-8")?
        .to_owned())
}
