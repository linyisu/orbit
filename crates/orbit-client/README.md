# orbit-client

本地、SSH、WSL 连接、账号流程、附件传输与会话观察，供 TUI 和 GUI 共用。前端保留平台交互和视图状态。

## 工作区服务

SSH 复用系统 OpenSSH 配置与认证；WSL 需要调用环境提供 `wsl.exe`。客户端将同架构静态服务部署到目标缓存，校验后复用，目标需提供工具执行依赖。

`--config` 与 `--auth-dir` 属于本机，`--database` 属于目标端。配置及引用的凭据通过连接传入目标的私有临时目录，远端 OAuth 刷新尚不回写本机。

断开客户端不会取消服务中的任务。重连按会话 ID 和事件游标恢复，未确认的副作用请求不自动重放。
GUI 与 TUI 共用 `windows` 的标签、选中项和草稿快照，按本机配置、认证及数据库路径区分启动配置。
启动恢复最近保存的窗口记录；多个窗口的不同草稿分别保留，附件保存为持久副本。恢复不自动发送消息或重新执行任务。
请求错误区分发送前编码失败、服务拒绝和传输失效；前两者保留连接，未确认的请求仍不重放。
缺失附件通过显式协议结果处理，旧服务返回的错误不按文本猜测。
DeepSeek 用量只展示 CNY：底部显示总余额，详情保留总余额、赠送余额和充值余额；原始查询结果保留其他币种。
连接配置使用声明与缓存的模型目录；显式刷新在后台获取目录，不阻塞会话取消和状态读取。
切换模型后采用服务端最新上下文占用快照；旧服务按新模型重新校准事件估计，不复用其他模型的实际用量。
账号初始化保留凭据身份，补填项目不会改写其他账号；新增账号分配独立名称，重新授权只更新引用该凭据的连接。API Key 验证、保存与目录缓存由共享流程完成；保存成功与目录刷新诊断分别返回。

## 全局扩展库

技能与插件市场始终在客户端主机管理，与当前工作区、目标数据库和模型连接无关。
`library` 在本机调用无会话的服务接口；应用时传输有界包快照到指定会话，保留脚本、参考资料和图片等资源，但不执行安装内容。
GUI 与 TUI 使用同一路由。全局保存成功不依赖远程会话在线；应用操作不自动重发。

## 本机语音

`local-speech` 功能通过 [speech.rs](src/speech.rs) 提供本机 CPU 转写。
[speech/model.rs](src/speech/model.rs) 固定模型来源、版本、大小与 SHA-256：
[Paraformer 中文、英文流式模型](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-paraformer-bilingual-zh-en)
逐块提供实时预览，[Qwen3-ASR 0.6B](https://huggingface.co/Qwen/Qwen3-ASR-0.6B)
在句尾根据对应音频校正文字和标点。
Qwen 使用非量化 FP32 ONNX 权重，避免 INT8 在中英混说上的实测退化。首次合计下载约 4 GB，
已有 Paraformer 缓存直接复用；Qwen 缓存子目录为 `qwen3-asr-0.6b-fp32`。
两者通过 [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) 在后台运行，不要求 Python 或云端识别服务。

缓存位于 `$XDG_CACHE_HOME/orbit/speech`，缺省根目录为 `~/.cache`，独立于项目与 SSH/WSL 目标。
准备阶段区分缓存校验、累计下载进度和两个模型的加载；取消下载保留固定版本的 `.part` 文件，
重试使用 HTTP Range 继续，服务端忽略 Range 时安全重新下载。完整文件通过大小和 SHA-256
校验后原子替换缓存；损坏下载不替换既有文件。跨进程准备使用可取消的缓存锁，已验证缓存离线可用。
取消加载立即结束调用方等待；无法中断的原生加载在后台结束并丢弃结果，重试不会与旧加载竞争。

音频按 16 kHz 单声道 PCM 输入，采集队列最多保留 3.2 秒待处理音频，单次最多 120 秒。
实时解码与串行句尾校正在不同工作线程运行，每句音频只校正一次，不反复识别越来越长的录音前缀。
停止录音会补齐预览并等待校正，较早句子的结果不能覆盖后续语音。取消丢弃后续结果，
失败保留已经发布的预览但不返回完整转写成功。音频不写入会话、工作区或网络请求。
GUI 只负责录音、进度展示和草稿插入，模型准备与转写逻辑不进入界面。

## 入口

- [conversation.rs](src/conversation.rs)、[activity.rs](src/activity.rs)、[execution.rs](src/execution.rs)：
  共享会话分段、进度状态、调用记录与详情页数据。分段保留思考、正文和调用的原始顺序，只合并连续工具；后台返回更新原记录，不改分组或位置。两端不自行解释工具结果。
  新收据的状态优先使用服务返回的明确结果；旧命令记录兼容原有格式。搜索详情支持匹配行及上下文、文件列表与计数。
  整理失败事件只结束整理阶段，不将运行映射为失败；运行终态仍由服务的失败或完成回执决定。
  `execution/details/operations` 投影任务、记忆、历史、证据和后台命令的必要结果；ReadFile 将源码与原始行号分离。`execution/details/request` 按用途投影命令、路径、查询和替换差异，未知工具保留完整调用内容；界面直接使用服务记录的工具名称。`execution/details/retained` 在独立连接上拼接已保存的完整命令日志、网页快照；运行中的 stdout/stderr 使用服务提供的原始字节游标续读，保留已加载前缀并拒绝过期响应；不扩大模型上下文、不重新执行工具。
- [lib.rs](src/lib.rs)、[connect.rs](src/connect.rs)：准备连接与打开会话。
- [library.rs](src/library.rs)：本机全局库访问、所选扩展快照同步与会话应用。
- [transport.rs](src/transport.rs)、[bootstrap.rs](src/bootstrap.rs)：传输、部署与握手。
- [pump.rs](src/pump.rs)、[prompt.rs](src/prompt.rs)：交互请求和单次任务事件流。
- [accounts.rs](src/accounts.rs)：账号授权与连接元数据；浏览器及项目输入由前端呈现。
- [configuration.rs](src/configuration.rs)：本机配置快照与凭据传递。
- [directories.rs](src/directories.rs)：目标目录读取、父目录与子目录路径，不通过客户端文件系统解释远端路径。
- [windows.rs](src/windows.rs)：两端共用的窗口恢复、草稿资产、进程租约与旧快照迁移。
- [attachments.rs](src/attachments.rs)：图片与普通文件的有界上传、粘贴路径识别；普通文件单个上限 20 MiB。
  文本预览保留 UTF-8 原文，最多显示前 1 MiB；二进制文件只提供元数据。

## 验证

```sh
cargo test -p orbit-client
```

[项目总览](../../README.md)
