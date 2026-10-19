# orbit-client

本地、SSH、WSL 连接、账号流程、附件传输与会话观察，供 TUI 和 GUI 共用。前端保留平台交互和视图状态。

## 工作区服务

SSH 复用系统 OpenSSH 配置与认证；WSL 需要调用环境提供 `wsl.exe`。客户端将同架构静态服务部署到目标缓存，校验后复用，目标需提供工具执行依赖。

Windows SSH 使用本机 OpenSSH，密码提示经私有命名管道传回客户端，不依赖 shell 脚本或
WSL。Windows 不使用 OpenSSH 不支持的 ControlMaster；Unix 保留已有的连接复用与期限。

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
[speech/model.rs](src/speech/model.rs) 固定官方 [Qwen3-ASR 0.6B](https://huggingface.co/Qwen/Qwen3-ASR-0.6B)
权重来源、版本、大小与 SHA-256。Qwen 直接生成初稿、修订暂定尾部并在句尾收尾，
不再加载 Paraformer，也不调用整句离线识别反复重跑历史录音。
首次下载原始 BF16 权重与词表约 1.88 GB，缓存子目录为 `qwen3-asr-0.6b`。
旧版 ONNX 缓存不再使用，也不自动删除。

推理由带补丁的 [qwen-asr](../../vendor/qwen-asr/ORBIT_PATCH.md) 在后台运行，不要求 Python。
Linux 静态链接 OpenBLAS，macOS 使用 Accelerate；Linux 源码构建需要 C 编译器、make，
首次构建下载 OpenBLAS 源码。x86_64 要求 AVX2 与 FMA，运行前检测，不能仅凭编译成功认定兼容。
编码和预填充使用浮点运算，后端单 token 解码使用内部 INT8 矩阵向量内核；
准确率必须以增量路径的实测为准。Linux 上内核池限制为两个工作线程，OpenBLAS 单线程。
所有本机模型操作在后台串行进入不可重入的原生内核池，等待期间可取消。

缓存位于 `$XDG_CACHE_HOME/orbit/speech`，缺省根目录在 Unix 为 `~/.cache`、Windows 为
`LOCALAPPDATA`，独立于项目与 SSH/WSL 目标；Windows 不要求 HOME 或 XDG 环境变量。
准备阶段区分缓存校验、累计下载进度和模型加载；取消下载保留固定版本的 `.part` 文件，
重试使用 HTTP Range 继续，服务端忽略 Range 时安全重新下载。完整文件通过大小和 SHA-256
校验后原子替换缓存；损坏下载不替换既有文件。跨进程准备使用可取消的缓存锁，已验证缓存离线可用。
取消加载立即结束调用方等待；无法中断的原生加载在后台结束并丢弃结果，重试不会与旧加载竞争。

音频按 16 kHz 单声道 PCM 输入，[speech/input.rs](src/speech/input.rs) 按采样数维护采集队列，
容量不受麦克风回调包大小影响。
待处理音频最多保留 30 秒，短暂推理停顿不会中断录音；持续积压超过此容量才明确报告过载，
不会静默丢弃音频。录音没有固定时长上限，停止时继续处理已接收的音频和不足一包的尾音。
每两秒解码一次，已完成的两秒音频窗口只编码一次，匹配的解码前缀复用 KV 缓存。
最多约 14 秒重锚，及时移除已释放编码窗口对应的 PCM 并重定位内部采样游标，
保留文字及 KV 状态，不重跑历史音频；保留末尾 12 个 token 可修订。
UTF-8 跨 token 拼接后再展示，暂定尾部整体替换，稳定前缀不会重复追加。
静音等待仅保留 200 ms 预录，连续 600 ms 静音后补齐当前句并释放该句缓存。
停止时补齐未满窗口及回滚尾部，即使恰好位于窗口边界也不丢尾字。
取消不再发布结果；已开始的单次原生计算在后台结束。失败保留已发布预览，不返回完整转写成功。
音频不写入会话、工作区或网络请求。GUI 只负责录音、音量反馈、进度展示和草稿插入。

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
- [platform/editor.rs](src/platform/editor.rs)：外部编辑器直接接收临时草稿路径，不隐式启动 shell。
  Windows 使用原生参数引用规则，保留路径反斜杠；含空格的程序路径用双引号包裹。Unix 保留 shell words 规则。
- [directories.rs](src/directories.rs)：目标目录读取、父目录与子目录路径，不通过客户端文件系统解释远端路径。
- [windows.rs](src/windows.rs)：两端共用的窗口恢复、草稿资产、进程租约与旧快照迁移。
- [attachments.rs](src/attachments.rs)：图片与普通文件的有界上传、粘贴路径识别；普通文件单个上限 20 MiB。
  文本预览保留 UTF-8 原文，最多显示前 1 MiB；二进制文件只提供元数据。

## 验证

```sh
cargo test -p orbit-client
```

会话控制测试使用 Cargo 一同构建的 Rust 替身
[session_peer.rs](tests/support/session_peer.rs)，不依赖 Python 或 shell 启动脚本。

[项目总览](../../README.md)

本机服务资产按客户端平台选择，Windows 使用 `.exe` 与私有命名管道，macOS 使用原生
可执行文件与 Unix socket。Linux 静态远端资产独立注册；非 Linux 构建可通过
`ORBIT_REMOTE_WORKSPACE_SERVER` 内置此资产，否则 SSH/WSL 使用显式上传或已安装服务。
