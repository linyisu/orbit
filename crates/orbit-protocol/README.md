# orbit-protocol

工作区请求、响应、事件、版本协商和有界长度分帧。使用 Serde 编解码，不依赖运行时、存储或界面。

## 入口

- [lib.rs](src/lib.rs)：核心类型、协议版本与帧读写。
- [library.rs](src/library.rs)：全局管理操作、插件选择和可传输的扩展包快照。
- [resources.rs](src/resources.rs)、[files.rs](src/files.rs)、[project.rs](src/project.rs)：资源、文件与项目操作。
- [plan.rs](src/plan.rs)、[subagents.rs](src/subagents.rs)、[user_input.rs](src/user_input.rs)：计划、子 Agent 与问答。
- [search.rs](src/search.rs)：选择器与补全的共用匹配规则。

全局扩展管理使用无会话的 `Library` 请求；应用与移除以会话 ID 定位，包内路径不引用客户端文件系统。
工作区写操作携带会话及工作区身份，服务校验过期请求。更改协议时同步检查客户端、服务端及兼容回归。`SubmitAttachments` 统一提交或暂存文件与图片，回放和队列分别保留两类附件元数据。
`ReadToolOutput` 按所属会话读取已保存的命令日志或网页快照，以字节游标分页；仅用于查看完整证据，不执行模型工具。`tool_output::Page.next_offset` 为空表示当前快照已读完，`read_offset` 在 EOF 仍保留原始字节位置，供增长日志续读；旧服务没有该字段时重新读取，不能用解码文字长度猜测字节位置。
`ToolProgress` 可携带两路输出预览及 `CommandProgress` 日志标识、原始字节数；缺少新字段的旧事件仍可读取。
`ReadFile.allow_missing` 默认关闭；仅明确启用的请求可收到 `FileMissing`，旧客户端仍收到原有错误响应。
`EncodedFrame` 在发送前完成编码和大小校验；开始写帧后发生取消或 I/O 错误，调用方需废弃连接。
用量摘要携带服务选定的展示标记，客户端不按供应商或模型名称猜测额度池；缺少标记的旧响应仍可读取。
会话配置可携带发布时的上下文占用快照；缺少该字段时，客户端兼容历史事件估计。
计划状态携带进入来源；旧状态缺少来源时按用户选择处理，不能由模型工具解除只读模式。

```sh
cargo test -p orbit-protocol
```

[项目总览](../../README.md)
