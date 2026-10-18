# 本地语音原生验收

使用 Xvfb、临时 HOME、配置、缓存和工作区。始终设置测试音源，不能使用用户麦克风
或真实录音。模型来自共享客户端指定的 Whisper 多语言 base 下载地址，文件大小
147951465 字节，SHA-256 见 `orbit-client/src/speech.rs`；不提交模型和音频到仓库。

用本地语音合成器生成独立样本，再转换为 16 kHz 单声道小端 PCM：

```sh
espeak-ng -v en-us -s 145 -w generated.wav \
  'Local speech recognition is working. Please keep this text in the message draft.'
ffmpeg -i generated.wav -ar 16000 -ac 1 -f s16le generated.pcm
```

构建带 `test-support` 的 GUI。设置 `ORBIT_GUI_SPEECH_TEST_PCM` 为样本的绝对路径，
让 GPUI 的 `AudioInput` 从样本按真实速率提供音频，其他录音状态和识别流程不替换。
把已校验的模型放在临时 `$XDG_CACHE_HOME/orbit/speech/ggml-base.bin`，以隔离测试服务启动：

1. 输入独立草稿，点击麦克风准备模型；完成后不会自动录音。再次点击开始，显示波形。
2. 样本播放完成后停止，确认本机转写插入当前输入位置，既有草稿保留，消息没有自动发送。
3. 再次开始后立即取消，草稿不变；转写期间切换标签、关闭任务或发送已有草稿，
   迟到的识别结果不能进入其他输入。
4. 使用不存在的测试 PCM 路径，确认错误有反馈且按钮可以重试，不打开真实麦克风。
5. 使用空缓存检查模型准备与下载进度、取消和重试。可切换任务，下载由本机模型服务共用。
6. 在浅色和深色、800×560 和宽窗口下检查输入区、模型选择、波形、取消与发送按钮。

真实模型回归默认忽略，需要显式提供独立生成的 PCM 和预先下载的缓存：

```sh
ORBIT_SPEECH_TEST_MODEL_DIRECTORY=/absolute/model-directory \
ORBIT_SPEECH_TEST_PCM=/absolute/generated.pcm \
cargo test -p orbit-client --features local-speech \
  local_model_transcribes_generated_speech_without_network -- --ignored
```

常规回归只使用生成的数据和本机回环 HTTP，覆盖下载完整性、损坏缓存、超限、取消与临时文件清理。
