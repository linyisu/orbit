# orbit-agent

模型与工具的执行循环，负责流式事件、补充输入、后台任务、取消和上下文整理。通过 trait 接入模型、工具与持久化，不持有界面或数据库连接。

## 入口

- [agent.rs](src/agent.rs)：运行循环和任务边界。
- [tool.rs](src/tool.rs)、[events.rs](src/events.rs)：工具注册与执行事件。
- [context.rs](src/context.rs)、[autocompact.rs](src/autocompact.rs)、[compaction.rs](src/compaction.rs)：预算估算、输出精简与摘要。
- [inbox.rs](src/inbox.rs)、[background.rs](src/background.rs)、[checkpoint.rs](src/checkpoint.rs)：补充消息、后台等待和检查点。

压缩前先归档原文，保留完整工具调用与回执配对。取消不会重放已完成的副作用；子 Agent 调度由 [orbit-service](../orbit-service/README.md) 负责。
已结束的最后一轮也可归档并摘要。系统指令、当前运行状态和最新用户请求保留原文；这些内容与工具定义超过窗口时，返回预算明细，不丢弃内容强行压缩。

工具通过 `ToolOutput` 的类型化结果和文件收据报告执行证据；模型输出文本不决定任务状态。
前台与后台完成事件都保存明确的完成、失败或取消结果；旧事件未提供结果字段时由读取边界兼容。
可选的 `_orbit_purpose` 随调用生成，注册器在参数校验和权限检查前提取为独立元数据；
旧调用及不支持扩展的动态 schema 使用参数摘要，不额外调用模型。
前台和后台错误共享结构化模型回执，保留错误类别和恢复建议；服务包装不能丢失已有取消或权限语义。

静态工具可以在 `impl Tool` 内使用 `#[orbit_agent::tool(name = "…")]`：
具名参数类型派生 `Deserialize` 和 `JsonSchema`，方法文档提供模型说明。
宏生成定义及参数解码，原有能力声明和执行函数继续负责授权约束、取消与结果收据。
旧事件缺少文件收据时，仅在恢复入口按旧格式读取。模型流的聚合与结束校验由 `orbit-model` 负责。

## 验证

```sh
cargo test -p orbit-agent
```

[项目总览](../../README.md)
