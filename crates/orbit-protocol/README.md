# orbit-protocol

工作区请求、响应、事件、版本协商和有界长度分帧。使用 Serde 编解码，不依赖运行时、存储或界面。

## 入口

- [lib.rs](src/lib.rs)：核心类型、协议版本与帧读写。
- [library.rs](src/library.rs)：全局管理操作、插件选择和可传输的扩展包快照。
- [resources.rs](src/resources.rs)、[files.rs](src/files.rs)、[project.rs](src/project.rs)：资源、文件与项目操作。
- [plan.rs](src/plan.rs)、[subagents.rs](src/subagents.rs)、[user_input.rs](src/user_input.rs)：计划、子 Agent 与问答。
- [review.rs](src/review.rs)：工作区变更、暂存回执与只读提交历史、历史文件查询。
- [search.rs](src/search.rs)：选择器与补全的共用匹配规则。

全局扩展管理使用无会话的 `Library` 请求；应用与移除以会话 ID 定位，包内路径不引用客户端文件系统。
工作区写操作携带会话及工作区身份，服务校验过期请求。更改协议时同步检查客户端、服务端及兼容回归。`SubmitAttachments` 统一提交或暂存文件与图片，回放和队列分别保留两类附件元数据。
`ReadToolOutput` 按所属会话读取已保存的命令日志或网页快照，以字节游标分页；仅用于查看完整证据，不执行模型工具。`tool_output::Page.next_offset` 为空表示当前快照已读完，`read_offset` 在 EOF 仍保留原始字节位置，供增长日志续读；旧服务没有该字段时重新读取，不能用解码文字长度猜测字节位置。
`ToolProgress` 可携带两路输出预览及 `CommandProgress` 日志标识、原始字节数；缺少新字段的旧事件仍可读取。
`ReadFile.allow_missing` 默认关闭；仅明确启用的请求可收到 `FileMissing`，旧客户端仍收到原有错误响应。
`EncodedFrame` 在发送前完成编码和大小校验；开始写帧后发生取消或 I/O 错误，调用方需废弃连接。
用量摘要携带服务选定的展示标记，客户端不按供应商或模型名称猜测额度池；缺少标记的旧响应仍可读取。
会话配置可携带发布时的上下文占用快照；缺少该字段时，客户端兼容历史事件估计。
`Browse` 只打开会话数据，`Prepare` 在发送前解析执行配置；准备失败不关闭浏览会话。
远程 `Browse` 同时携带独立权限快照；捕获或校验失败仍可读历史，但文件操作和执行明确拒绝。
快照在转入执行后继续保留，不能退回目标端的默认权限；旧端缺少该能力时要求升级。
`SessionInfo.phase` 区分浏览与执行就绪，旧服务缺少该字段时按就绪处理；浏览阶段的容量字段为 0，
表示尚未解析执行预算，界面显示未知，不能用来发起模型请求或计算占用百分比。
`InspectUsage` 无需模型 actor 即可查询已选择账号与会话用量，远程配置仍通过私有快照传递。
计划状态携带进入来源；旧状态缺少来源时按用户选择处理，不能由模型工具解除只读模式。

```sh
cargo test -p orbit-protocol
```

[项目总览](../../README.md)

`QueueInput` 暂存文本及附件到下一轮，队列项通过附加 `delivery` 字段区分下一轮发送与当前轮次引导，既有状态和旧请求语义保留；`InputQueue::Steer` 按消息原文和运行编号校验后提升为引导输入。旧 `Steer` 请求保持原有即时引导语义。
