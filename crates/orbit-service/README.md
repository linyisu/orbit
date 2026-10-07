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

同一目标用户下，数据库的规范路径与协议版本确定共享服务地址。同一构建的程序、不同配置文件和工作目录共享服务。每个连接携带配置与认证路径，用于创建会话；打开已有会话时接入它的 actor，并沿用该会话的模型、权限与工作目录。

连接接入核对后台进程的实际可执行文件和构建代次。同一数据库的终端共享协议兼容的最新版服务，旧客户端复用较新的后台。升级通过服务协议请求空闲关闭，再启动新服务；启动锁串行协调切换。运行任务结束后完成切换。已连接的 TUI 标签断线时自动重连原会话，并从持久化事件恢复进度；草稿和待发送图片保留，尚未确认的请求由用户决定是否重试。构建代次使用编译时间，发布构建可通过 `SOURCE_DATE_EPOCH` 提供可复现的时间。

多个终端可以同时打开同一会话，各自按事件游标同步消息、运行状态、模型设置和审批。服务按会话串行处理操作，任务运行期间接受补充指令、审批和取消，重复请求通过请求 ID 去重。写入锁由服务持有，保护会话存储；客户端断开后任务继续，重连接收已有事件。服务重启后将未完成执行标记为中断，副作用命令由用户明确决定是否重试。

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

## 用户输入请求

`user_input` 管理 Agent 发起的结构化问答。`ask_user` 使用题目 ID 和选项 ID 表示答案，支持 `single`、`multiple`、`text` 题型、自定义答案及补充说明。每组最多 8 题，建议模型集中询问 1～4 个关联问题。

TUI 连接后通过 `EnableUserInput` 声明交互能力。会话的 `Page.user_inputs` 提供待回答快照，`AnswerUserInput` 提交答案或明确取消，`UserInputHistory` 分页读取记录。服务先持久化结果再恢复工具调用；相同提交可以重试，冲突答案返回已处理提示。问答独立于审批模式，等待用户期间保持任务活跃。

子代理通过父会话的问答服务发布问题，记录中携带来源会话 ID。客户端断开后，服务保留待回答请求。任务取消会结束对应问答；服务重启后，遗留待回答记录标记为 `interrupted`，历史问题和已经提交的答案继续保留。未声明交互能力的会话调用问答工具会收到明确错误。
