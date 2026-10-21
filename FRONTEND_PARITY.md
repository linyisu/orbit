# 客户端功能对照

TUI、GUI 和 Android 通过共享工作区服务使用会话与工具。文件、任务和运行状态属于目标环境；
客户端只持有交互状态和界面偏好。

## 功能入口

| 能力 | TUI | GUI | Android |
| --- | --- | --- | --- |
| 账号授权与配置 | `/connect` | 账号设置 | 由电脑端配置 |
| 模型选择 | `/model`、`/model refresh` | 输入区模型菜单、账号设置 | 工作区模型页 |
| 项目与会话 | `/new`、`/resume`、`/ssh`、`/wsl` | 项目侧栏、标签、连接向导 | 项目会话抽屉、新任务项目选择 |
| 文件与 Git | `/file`、`/review`、`/changes` | 文件、变更和历史面板 | 工作区文件、变更和历史页 |
| 任务与计划 | `/tasks`、`/plan`、`/plan view` | 项目任务页、计划面板 | 工作区任务和计划页 |
| 待发送消息 | `/queue` | 输入区上方队列 | 工作区待发送消息页 |
| 技能与插件 | `/skills`、`/plugins` | 技能设置、插件市场 | 工作区技能和插件页 |
| MCP | `/mcp`、`/mcp status` | MCP 设置、导入、会话应用 | 工作区 MCP 页 |
| 问答与子 Agent | 问答弹窗、子 Agent 面板 | 问答卡片、Agent 面板 | 问答及工作区子 Agent 页 |
| 工具详情 | 点击或 Alt+O | 点击打开详情区 | 点击打开详情页 |
| 本地语音输入 | 无入口 | 输入区麦克风 | 无入口 |
| 手机接入管理 | `/phone` | 设置中的“手机连接” | 扫码或连接已保存电脑 |

Android 操作已授权电脑上的项目和会话，支持附件上传；账号授权、SSH/WSL 连接管理和完整配置编辑
由桌面或终端客户端提供。Android 构建与配对见 [mobile](mobile/README.md)。

## 会话、附件与恢复

思考、说明、工具和答复按时间顺序投影，仅合并连续工具。GUI 的详情区可与对话并排，
窄窗口提供返回对话的完整视图；TUI 使用键盘弹窗，Android 使用独立详情页。
客户端共用结果契约，前端维护折叠、焦点和阅读位置。

GUI 和 TUI 通过 `orbit-client::windows` 恢复标签、选中项和草稿；TUI 的 `/restore` 提供手动入口。
Android 按已保存电脑恢复连接、会话和文本草稿。恢复不自动发送消息或重放结果未知的写请求。
关闭单个标签不等于停止任务，主机服务退出规则见 [生命周期](crates/orbit-service/README.md#生命周期)。

GUI 支持图文队列及撤回编辑，TUI 使用 `/queue` 管理；计划批准、问答和工具审批相互独立。
GUI 提供系统字体、图片灯箱和只读预览；TUI 使用终端字体及图片协议，`/editor` 调用外部编辑器。

## 扩展与远程授权

技能和插件市场属于客户端主机的全局库。显式应用的完整包快照保存到所选会话，
其他会话不自动启用；全局编辑、市场刷新和移除不会改写已应用快照。
MCP 可应用到已有会话，高级 Agent 配置用于新会话。

GUI 和 TUI 通过 `orbit-client::phone` 管理手机接入。电脑级授权可选择不同项目，
项目级授权限于其指定范围；手机不接收主机模型凭据。电脑级配对与连接方式见
[Android 配对](mobile/README.md#配对与使用)。

## 共享代码边界

| 职责 | 所属模块 |
| --- | --- |
| 账号授权、连接准备与恢复 | `orbit-client::accounts`、`connect`、`transport` |
| 全局扩展、包传输与会话应用 | `orbit-client::library`、`orbit-service::workspace::library` |
| 会话轮询、请求分流与回执 | `orbit-client::pump` |
| 会话分段、工具摘要及详情 | `orbit-client::conversation`、`activity`、`execution`、`remote::presentation` |
| 窗口恢复、草稿与附件副本 | `orbit-client::windows`、`remote::vault` |
| 项目置顶与排序 | `orbit-client::projects` |
| 本机语音模型与离线转写 | `orbit-client::speech`，`local-speech` feature |
| 附件传输与队列资产恢复 | `orbit-client::attachments`、`pump`、`remote` |
| 模型发现、预算与连接校验 | `orbit-config::config::connections` |
| Agent 与 MCP 配置解析 | `orbit-config::config::advanced`、`connections::mcp` |
| 协议与输入约束 | `orbit-protocol`、`orbit-client::protocol` |
| 语法识别 | `orbit-syntax` |

终端转义、系统文件选择器和 GPUI HTTP 适配负责平台转换；认证、权限、网络和文件边界由共享层处理。
`cargo xtask check` 覆盖普通、开发及平台依赖。使用说明见 [TUI](crates/orbit-tui/README.md)
和 [GUI](crates/orbit-gui/README.md)。
