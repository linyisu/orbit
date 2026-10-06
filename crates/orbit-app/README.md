# orbit-app

[项目总览](../../README.md)

`orbit-app` 是 Orbit 的应用服务层，协调配置、模型选择、会话、工具、子代理和工作区生命周期。CLI、TUI 和无界面服务均通过本库运行任务。

## 职责与接口

| 模块 | 内容 |
| --- | --- |
| [config.rs](src/config.rs)、[config/connections.rs](src/config/connections.rs) | 配置解析、校验与连接保存 |
| [models/](src/models) | 模型选择项、账号目录发现和公共规格补充 |
| [context/](src/context) | 项目指令、历史归档、任务和会话记忆 |
| [extensions/](src/extensions) | Skill 发现、插件市场与启用状态 |
| [session.rs](src/session.rs)、[session/title.rs](src/session/title.rs) | 会话写入锁、恢复、检查点与标题任务 |
| [tools.rs](src/tools.rs)、[tools/](src/tools) | 工具装配、工作区权限、命令输出和核验 |
| [execution.rs](src/execution.rs) | Agent 启动、手动压缩、运行结果与取消处理 |
| [chat.rs](src/chat.rs) | 单次任务选项、能力策略与事件回调 |
| [subagents.rs](src/subagents.rs)、[side_questions.rs](src/side_questions.rs) | 子代理调度和旁路请求 |
| [workspace/](src/workspace) | 工作区准备、传输、部署、协议与服务 |
| [platform/](src/platform)、[platform.rs](src/platform.rs) | 外部编辑器、浏览器与隔离 worker 入口 |

`prepare_workspace` 返回会话和运行资源，`prepare_workspace_resources` 按指定目录准备模型、工具、指令与权限。`execution::start` 接收会话、领域消息和运行选项，返回事件接收器、取消令牌和任务句柄。本地与服务端复用这一接口。`chat::run` 通过回调交付领域事件和诊断，返回回答、模型名与可选补丁。

`workspace::prompt::run` 提交远端任务、轮询事件并处理取消和审批策略，通过回调交付输出；`workspace::pump` 驱动交互客户端的请求与事件通道。模型目录刷新诊断由工作区资源返回给调用方。

应用层依赖 `orbit-agent`、`orbit-model`、`orbit-auth`、`orbit-runtime`、`orbit-tools` 和 `orbit-store`。绘制、键鼠事件、历史显示、参数解析与终端输出属于前端。

## 工作区服务

本地进程、系统 OpenSSH 和 `wsl.exe` 使用同一请求协议。SSH 沿用 `~/.ssh/config`、跳板机、密钥代理与主机校验配置；TUI 的私有 askpass 通道处理指纹确认、密码、私钥口令和交互认证。WSL 在选定发行版中启动 Linux 服务。当前客户端依赖 Linux/Unix 接口，WSL 适配器要求调用环境能执行 `wsl.exe`。

部署服务检测目标系统和架构，将客户端提供的同架构静态程序上传至 `~/.cache/orbit/servers/<SHA-256>/orbit`。传输完成后校验并原子安装，重复连接复用同版本文件。服务在握手前执行运行检查。`--server` 支持目标端预装程序，`--upload-server` 支持指定兼容程序；自动部署的架构需与客户端一致。

SSH/WSL 默认使用本地配置、模型目录和配置引用的凭据。TUI 连接还继承当前模型。配置通过认证连接传入，存放在服务私有临时目录；目录权限为 0700，文件权限为 0600，正常退出时清理。`--config` 和 `--auth-dir` 指本地路径，`--database` 指目标路径。模型、工具、MCP、Skill 和项目指令在目标环境执行或发现，目标需提供相应网络与运行依赖。

本地 Rust 工具链路径在目标不存在时，配置安装过程查找目标 rustup 默认工具链或系统工具链。命令隔离需要目标提供 `bwrap`、`prlimit` 和可用的用户命名空间。

服务支持历史恢复、流式事件、审批、取消、模型切换、文件读取和子代理详情。客户端断开后任务继续运行，通过同一目标和会话 ID 接回。服务空闲 15 分钟退出，`--stop` 停止空闲服务。运行时的会话写入锁由服务持有。

请求 ID 与内容指纹保存在数据库中，用于重复投递确认。客户端按游标分页接收持久事件。服务退出或确认中断时，客户端提示检查历史；恢复会话标记中断任务，并由用户决定后续操作。协议输出和服务诊断使用独立通道。

## 模型目录

模型接口提供的能力用于更新模型目录；对于只返回模型 ID 的官方 DeepSeek 接口，Orbit 为 `deepseek-flash` 和 `deepseek-v4-pro` 补充已核对的 `none / low / high / max` 思考档位，默认 `high`。手动管理的模型列表同样获得这些默认能力，显式配置的思考档位优先；其他端点和未知模型的思考协议需由服务端或连接高级设置提供能力。

TUI 每秒检测配置文件变化，并每 15 分钟异步刷新所有连接的模型目录（账号登录、API Key 及无需认证的自定义服务）；`/model refresh` 可立即刷新并打开模型选择器。能力变化同步到所有标签，保留仍受支持的当前思考档位；正在进行的回复使用原客户端和额度，结束后应用更新。自动更新不会打开选择器。

各连接独立刷新，失败时保留对应缓存或现有目录；缓存按服务地址、协议和认证身份隔离。普通 API 会读取完整分页，并保留接口提供的能力信息；Claude 的容量、图像输入与 adaptive thinking 思考档位直接来自模型接口。Google Gemini CLI 从配额接口补充模型 ID，缺少某个配额项不会删除已配置模型。

公开模型规格来自 [models.dev](https://models.dev/)，由 [model_metadata.rs](src/models/model_metadata.rs) 补齐 API 模型缺少的上下文、输入与输出容量、视觉能力，以及已支持的官方思考档位。服务地址和模型 ID 必须精确匹配；共用地址的目录若对同一模型给出冲突规格，则不自动采用。优先级为显式配置、账号或 API 返回的能力、公共规格；公共规格不会新增可用模型，也不代表订阅额度或访问权限。输出容量仅作为上限，不提高用户设置的输出预算。

公共规格缓存位于认证目录下的 `model-metadata/catalog.json`，有效期 24 小时。刷新复用网页工具的代理和地址检查，不发送账号凭据，等待最多三秒；失败时使用旧缓存或 [内置快照](src/models/model_metadata.json)。所有 API 连接均关闭 `auto_refresh_models` 时不联网更新规格。维护者可运行 `orbit model-metadata crates/orbit-app/src/models/model_metadata.json` 更新内置快照；导出命令遵循 `[web]` 配置，保留来源信息；MIT 许可见 [LICENSE.models-dev](src/models/LICENSE.models-dev)，不需要登录。快照按模型逐行保存，方便审查数据差异。

[账号起始目录](src/models/account_models/bootstrap.json)作为 OAuth 账号首次连接时的离线起始目录；后续模型可用性由账号接口刷新，API 模型由对应服务的发现接口提供，不能用初始目录推断全部支持模型。

通过 `/connect` 手动启用的列表（`managed_models = true`）保持模型选择与显式能力设置，仍同步这些模型缺少的能力；自动目录会同步新增与下线的模型。服务不提供模型目录时，可在对应 `[providers.<名称>]` 下设置 `auto_refresh_models = false`，继续使用手动配置。

## 扩展与上下文

启动和切换工作区时发现用户与项目的 `SKILL.md`，以及项目启用的插件技能。模型只在存在可用技能时获得 `load_skill` 工具与名称目录；完整内容按需加载。路径和优先级见 [TUI 使用说明](../orbit-tui/README.md)及 [工具文档](../orbit-tools/README.md)。

子代理使用独立的会话、只读工具和取消令牌，父任务通过工具注册接口创建、查询或继续子任务。旁路提问使用独立流式请求和单独的数据库记录；TUI 负责显示该请求的事件与历史。

任务和会话记忆操作通过 `context::project_state::actions` 更新存储，并刷新模型上下文。指令、历史归档与记忆各自维护所属领域的规则。

## 验证

```sh
cargo test --offline -p orbit-app
cargo check --offline -p orbit-app
```

测试覆盖配置、模型目录、会话锁与恢复、工具组合、子代理、旁路请求和工作区传输。应用库可独立构建。
