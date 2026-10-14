# orbit-agent

模型与工具的执行循环，负责流式事件、补充输入、后台任务、取消和上下文整理。通过 trait 接入模型、工具与持久化，不持有界面或数据库连接。

## 入口

- [agent.rs](src/agent.rs)：运行循环和任务边界。
- [tool.rs](src/tool.rs)、[events.rs](src/events.rs)：工具注册与执行事件。
- [context.rs](src/context.rs)、[autocompact.rs](src/autocompact.rs)、[compaction.rs](src/compaction.rs)：预算估算、输出精简与摘要。
- [inbox.rs](src/inbox.rs)、[background.rs](src/background.rs)、[checkpoint.rs](src/checkpoint.rs)：补充消息、后台等待和检查点。

压缩前先归档原文，保留完整工具调用与回执配对。取消不会重放已完成的副作用；子 Agent 调度由 [orbit-service](../orbit-service/README.md) 负责。

## 验证

```sh
cargo test -p orbit-agent
```

[项目总览](../../README.md)
