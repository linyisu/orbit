# orbit-service

[项目总览](../../README.md)

`orbit-service` 是 Orbit 的应用服务层，协调配置、模型选择、会话、工具、子代理和工作区生命周期。`orbit-server` 接收客户端请求，并由本库管理目标端会话与任务。

## 职责与接口

| 模块 | 内容 |
| --- | --- |
| [context/](src/context) | 项目指令、历史归档、任务和会话记忆 |
| [extensions/](src/extensions) | Skill 发现、插件市场与启用状态 |
| [session.rs](src/session.rs)、[session/title.rs](src/session/title.rs) | 会话写入锁、恢复、检查点与标题任务 |
| [tools.rs](src/tools.rs)、[tools/](src/tools) | 工具装配、工作区权限、命令输出和核验 |
| [execution.rs](src/execution.rs) | Agent 启动、手动压缩、运行结果与取消处理 |
| [policy.rs](src/policy.rs) | 会话能力上限、只读策略与模型选择 |
| [subagents.rs](src/subagents.rs)、[side_questions.rs](src/side_questions.rs) | 子代理调度和旁路请求 |
| [workspace/](src/workspace) | 工作区准备、会话请求、审批与事件分页 |
| [platform.rs](src/platform.rs) | 文档与浏览器 worker 入口 |

`workspace` 持有会话 actor、审批、运行任务与事件游标。客户端请求覆盖模型切换、运行中补充指令、上下文压缩、撤销、附件、旁路提问、子代理、项目任务、会话记忆和插件。目标端解析工作区路径并提供目录及历史补全。`execution::start` 在服务内启动 Agent，返回事件、取消令牌和任务句柄。

同一服务可持有多个目录的会话，写入锁按会话隔离。客户端断开后任务继续；重新打开会话时读取持久记录并补齐事件。服务重启后将未完成执行标记为中断，副作用命令由用户明确决定是否重试。

客户端连接、部署与事件订阅由 `orbit-client` 提供；协议类型由 `orbit-protocol` 定义。配置和模型目录由 `orbit-config` 管理。

应用层依赖 `orbit-agent`、`orbit-model`、`orbit-auth`、`orbit-runtime`、`orbit-tools` 和 `orbit-store`。绘制、键鼠事件、历史显示、参数解析与终端输出属于前端。

## 扩展与上下文

启动和切换工作区时发现用户与项目的 `SKILL.md`，以及项目启用的插件技能。模型只在存在可用技能时获得 `load_skill` 工具与名称目录；完整内容按需加载。路径和优先级见 [TUI 使用说明](../orbit-tui/README.md)及 [工具文档](../orbit-tools/README.md)。

子代理使用独立会话和取消令牌；探索与审查使用只读工具，开发使用独立 worktree 与父会话授权的工具，父任务通过工具注册接口创建、查询或继续子任务。旁路提问使用独立流式请求和单独的数据库记录；TUI 负责显示该请求的事件与历史。

任务和会话记忆操作通过 `context::project_state::actions` 更新存储，并刷新模型上下文。指令、历史归档与记忆各自维护所属领域的规则。

## 验证

```sh
cargo test --offline -p orbit-service
cargo check --offline -p orbit-service
```

测试覆盖配置、模型目录、会话锁与恢复、工具组合、子代理、旁路请求和工作区传输。应用库可独立构建。
