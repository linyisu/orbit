# GUI 与 TUI 功能对照

两端通过同一工作区服务执行任务；本地、SSH、WSL 的文件与运行状态均属于目标环境。

| 能力 | TUI | GUI |
| --- | --- | --- |
| 账号与模型 | `/connect`、`/model` | 账号设置、输入区模型菜单 |
| 项目与会话 | `/new`、`/resume`、`/ssh`、`/wsl` | 项目侧栏、标签、连接向导 |
| 文件与 Git | `/file`、`/review`、`/changes` | 文件浏览器、变更面板 |
| 任务与计划 | `/tasks`、`/plan`、`/plan view` | 项目任务页、计划面板 |
| 暂存输入 | `/queue` | 输入框上方暂存消息 |
| 技能与插件 | `/skills`、`/plugins` | 技能设置、插件市场 |
| MCP | `/mcp`、`/mcp status` | MCP 设置、来源导入、应用到当前会话 |
| 问答与子 Agent | 问答弹窗、子 Agent 面板 | 问答卡片、Agent 面板 |
| 工具过程与详情 | 按顺序分组的工具列表、Alt+O 详情弹窗、Alt+P 定位最新输出 | 按顺序分组的工具列表、按需详情区、输入区上方运行状态 |
| 输入历史与快捷键 | Ctrl+Up / Ctrl+Down、`/keys` | Up / Down、快捷键设置 |
| 本地语音输入 | 暂无入口 | 输入区麦克风，本机 Qwen 增量初稿、尾部修订与句尾收尾；后台准备支持进度、取消和续传 |

技能和插件市场属于本机全局库，两端共用 `orbit-client::library`。显式应用的完整资源快照在目标会话中持久保存，其他会话不自动启用。

配置与凭据在本机保存，工具在目标环境运行。MCP 可在 GUI 应用到已有会话；高级 Agent 配置用于新会话。问答、计划批准与工具审批相互独立。

关闭标签视图不等于停止任务，计划批准和未确认请求重发需显式操作。GUI 支持图文暂存，编辑时撤回到输入框；TUI 使用 `/queue` 管理队列。

GUI 提供系统字体、图片灯箱和只读文件预览；TUI 的字体与图片协议由终端决定，`/editor` 使用外部编辑器。两端启动时通过 `orbit-client::windows` 自动恢复标签和草稿，TUI 的 `/restore` 是手动补充入口。

构建与操作见 [TUI](crates/orbit-tui/README.md) 和 [GUI](crates/orbit-gui/README.md)。

## 代码边界

| 职责 | 所属模块 |
| --- | --- |
| 账号授权、连接准备与恢复 | `orbit-client::accounts`、`connect`、`transport` |
| 全局扩展管理、快照传输与会话应用 | `orbit-client::library`、`orbit-service::workspace::library` |
| 会话轮询、后台请求分流与回执 | `orbit-client::pump` |
| 思考与答复分段、工具摘要及详情数据 | `orbit-client::conversation`、`activity`、`execution` |
| 窗口恢复、草稿与附件副本 | `orbit-client::windows` |
| 本机语音模型缓存与离线转写 | `orbit-client::speech`（`local-speech` feature） |
| 附件识别、上传、下载与队列资产恢复 | `orbit-client::attachments`、`pump` |
| 模型发现、预算与连接编辑校验 | `orbit-config::config::connections` |
| Agent 设置与 MCP 表单解析 | `orbit-config::config::advanced`、`connections::mcp` |
| 协议数据与输入约束 | `orbit-protocol`、`orbit-client::protocol` |
| 语法识别 | `orbit-syntax` |

前端保留渲染、事件路由、焦点、滚动、选区、编辑草稿、视图缓存及界面偏好。
终端转义、系统文件选择器和 GPUI HTTP 适配只负责平台接口转换；认证、网络及文件边界由共享层处理。
依赖检查覆盖普通、开发和目标平台声明，防止前端重新引入执行层。

GUI 将工具输出、计划及子任务放入可关闭的详情区，宽窗口与对话并排，窄窗口提供返回对话的完整视图。TUI 保留键盘操作的详情弹窗，两端共用结果投影与服务契约。

项目浏览和历史审阅：GUI 从当前项目菜单打开文件树、Git 变更及提交历史，详情区域可关闭；TUI 保留 `/file`、`/review` 弹窗，并通过 `/review history [分支或提交]` 浏览提交及完整文件。两端使用同一 Review 协议，历史查询不会暂存或改写文件；会话写入快照与当前 Git 变更分开查看。

手机远程连接：GUI 使用设置中的“手机连接”，TUI 使用 `/phone` 弹窗；两端通过 `orbit-client::phone` 管理本机远程开启、扫码配对和设备撤销。电脑级授权可选择不同项目，项目级授权保持其指定范围。

Android 工作区已接入模型、审批模式、计划、文件读取、Git 审阅、任务、子 Agent、队列、技能、插件目录和记忆接口。账号授权、SSH/WSL 连接管理及完整配置编辑尚未与桌面齐平。
