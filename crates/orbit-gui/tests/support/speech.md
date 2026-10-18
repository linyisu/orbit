# 本地流式语音原生验收

使用 Xvfb、临时 HOME、配置、缓存和工作区。始终设置测试音源，不能使用用户麦克风
或真实录音。ASR 使用共享客户端指定的 Paraformer 实时模型和 Qwen3-ASR 0.6B 句尾校正模型；
Qwen 使用非量化 FP32 ONNX 权重，合计首次下载约 4 GB。
固定版本、大小和 SHA-256 见 `orbit-client/src/speech/model.rs`；不提交模型和音频到仓库。

使用独立的本地神经语音合成器生成普通话，不能只测英文。
例如使用 [Matcha Baker 与 Vocos](https://k2-fsa.github.io/sherpa/onnx/tts/all/Chinese/matcha-icefall-zh-baker.html)
合成以下自编文本，再转换为 16 kHz 单声道小端 PCM：

> 你好，现在开始测试。今天下午三点开会，请把报告保存在项目目录。

```sh
ffmpeg -i generated-mandarin.wav -ar 16000 -ac 1 -f s16le generated-mandarin.pcm
```

构建带 `test-support` 的 GUI。设置 `ORBIT_GUI_SPEECH_TEST_PCM` 为样本的绝对路径，
让 GPUI 的 `AudioInput` 从样本按真实速率提供音频，其他录音状态和识别流程不替换。
将两组已校验模型按 `model.rs` 的目录布局放在临时 `$XDG_CACHE_HOME/orbit/speech/`；
复用已校验的 Paraformer 缓存，不复制用户缓存或会话。合成音频末尾追加至少五秒静音，
让测试音源在用户停止录音前产生句尾边界。以隔离测试服务启动：

1. 输入独立草稿，点击麦克风准备模型；完成后不会自动录音。再次点击开始，底栏隐藏附件、模式和模型选择，显示左侧取消、贯穿剩余空间的长波形、右侧完成录音和发送。
2. 停止之前，中文文字已在输入位置实时显示；后续假设替换语音区段，不能重复追加。
   静音期间 Qwen 将前一整句替换为带标点的校正结果，麦克风仍保持打开。
   右侧方块完成录音后补齐尾部文字并完成校正，既有草稿保留，不自动发送，普通底栏恢复。
3. 左侧 × 取消录音或切换页面，语音预览回滚；再次打开会话时没有迟到文字。
   手工编辑草稿后停止后续语音覆盖，不回滚用户编辑。发送时保留当前可见文字并停止录音。
4. 测试 UTF-8 中文选区替换、取消恢复原选区、静音不删除选中文本。
5. 使用不存在的测试 PCM 路径，确认错误有反馈且按钮可以重试，不打开真实麦克风。
   收尾失败保留已显示文字，供用户审阅，不当作完整转写。
6. 使用空缓存检查多文件累计下载进度、取消和重试；确认 `.part` 保留且重试使用 Range 续传。
   缓存校验、加载实时模型、加载 Qwen 分别有状态提示；加载期间仍可编辑草稿和切换页面。
   取消加载立即退出准备状态，晚到结果不激活麦克风；重试成功，下载由本机模型服务共用。
7. 在浅色和深色、800×560 和宽窗口下检查输入区、波形、取消与发送按钮。

真实模型回归默认忽略，需要显式提供上述独立生成的中文 PCM 和预先下载的缓存：

```sh
ORBIT_SPEECH_TEST_MODEL_DIRECTORY=/absolute/model-directory \
ORBIT_SPEECH_TEST_PCM=/absolute/generated-mandarin.pcm \
cargo test -p orbit-client --features local-speech \
  speech::tests:: -- --ignored --test-threads=1
```

检查停止录音前已有中文输出、整句字符错误率不超过 10%、静音和取消。
合成语音验证不能替代真实麦克风、口音和噪声环境验收，不据此宣称真人准确率。
常规回归使用生成的数据和本机回环 HTTP，覆盖跨分片续传、服务端忽略 Range、错误范围和大小、
损坏缓存、原子替换、取消保留分片、缓存锁、采集超限与乱序句尾校正。
真实模型回归同时检查加载中取消能在 250 ms 内结束等待，随后重试成功。
