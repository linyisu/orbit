# Qwen 原生增量识别补丁

来源：[qwen-asr 0.11.0](https://crates.io/crates/qwen-asr/0.11.0)，
上游 [QwenASR](https://github.com/huanglizhuo/QwenASR) 提交
`d39208de340a8b0c3a28f53ac1fd160a7118666f` 的 `crates/qwen-asr`，MIT 许可见 LICENSE。
模型不随源码分发，固定官方权重来源由 orbit-client 的 speech/model.rs 维护。

仅调整原生增量路径：停止时即使没有新采样也提交回滚尾部；UTF-8 跨 token 拼接；
长录音每六个增量块重锚，先提交当前块，再清理对应的音频与输出 token 前缀；
回滚不得越过已确认文字，避免漏字、重复提交和切片越界。公开累计编码采样数、窗口数
和预填充复用计数用于真实增量验收。模型无关的边界回归位于 transcribe::orbit_stream_tests。

库只生成 Rust rlib。Linux 的 blas feature 静态依赖 openblas-src 0.10.16，避免用户机器
额外安装 BLAS；macOS 仍使用系统 Accelerate。线程上限及可移植 CPU 内核由根 Cargo 配置控制。
原始包其余源码和测试保留上游格式，完整差异见 patches/qwen-asr.patch。

包内 Cargo.lock 不参与构建；所有产品依赖由 workspace 根 Cargo.lock 固定。
