# orbit-service

应用服务层，协调会话、模型、工具、审批和子 Agent。CLI、TUI、GUI 通过同一协议使用本库。

## 入口

- [workspace/](src/workspace/)：服务生命周期、会话 actor、请求与事件分页。
- [session.rs](src/session.rs)、[execution.rs](src/execution.rs)：会话恢复、Agent 启动与运行结果。
- [tools.rs](src/tools.rs)、[planning.rs](src/planning.rs)、[user_input.rs](src/user_input.rs)：工具装配、计划与问答。
- [subagents.rs](src/subagents.rs)、[side_questions.rs](src/side_questions.rs)：子任务与旁路提问。
- [context/](src/context/)、[extensions/](src/extensions/)：指令、记忆、技能与插件。

## 生命周期

同一数据库由共享服务管理，会话 actor 串行处理状态变更。客户端断开后任务继续；服务重启后标记中断，不自动重跑副作用命令。

Plan 只允许只读操作，批准后进入 Build，工具审批仍生效。MCP 可异步应用到已有会话；当前运行保留原工具集，后续发送使用新工具。

技能和 MCP 导入先扫描来源，再按用户选择安装。配置、模型适配和持久化分别由对应 crate 负责。

## 验证

```sh
cargo test -p orbit-service
```

[项目总览](../../README.md)
