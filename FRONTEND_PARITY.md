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
| 输入历史与快捷键 | Ctrl+Up / Ctrl+Down、`/keys` | Up / Down、快捷键设置 |

配置与凭据在本机保存，工具在目标环境运行。MCP 可在 GUI 应用到已有会话；高级 Agent 配置用于新会话。问答、计划批准与工具审批相互独立。

关闭标签视图不等于停止任务，计划批准和未确认请求重发需显式操作。GUI 支持图文暂存，编辑时撤回到输入框；TUI 使用 `/queue` 管理队列。

GUI 提供原生字体、图片灯箱和编辑器；TUI 的字体与图片协议由终端决定，`/editor` 使用外部编辑器。GUI 自动恢复标签草稿，TUI 使用 `/restore`。

构建与操作见 [TUI](crates/orbit-tui/README.md) 和 [GUI](crates/orbit-gui/README.md)。
