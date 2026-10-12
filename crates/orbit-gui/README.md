# orbit-gui

[项目总览](../../README.md)

Orbit 原生桌面客户端，使用 GPUI Kit Component，外观面板使用 GPUI Component Shell。会话执行与数据库由工作区服务负责，GUI 通过 `orbit-client` 接入；账号设置复用 `orbit-auth` 与 `orbit-config`。

## 启动

目前支持本仓库的 Linux 执行环境。先按照根目录说明准备 Rust stable、musl 工具链和模型配置。Ubuntu 24.04 的 GUI 构建依赖：

```sh
sudo apt install build-essential clang pkg-config libfontconfig-dev libwayland-dev \
  libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev libasound2-dev libvulkan-dev
cargo run -p orbit-gui --locked -- --config config.toml
```

GUI 和 CLI/TUI 一样内嵌静态工作区服务。已有兼容服务时，构建可用 `ORBIT_WORKSPACE_SERVER=/absolute/path/to/orbit-workspace-server` 复用它；运行时也可用 `--server` 覆盖连接的服务程序。GUI 暂未加入便携打包，默认 Shell 目录仍从源码目录加载。

```sh
cargo run -p orbit-gui --locked -- \
  --config /path/to/config.toml \
  --workspace /path/to/project \
  --model provider::model \
  --resume SESSION_ID
```

`--model`、`--resume` 可省略：新会话优先使用上次选择的模型，再回退到可用模型；恢复会话使用服务保存的模型。认证沿用现有认证目录，也可指定 `--auth-dir`。`--database` 与其他客户端含义一致。

## 界面与操作

- 没有全局底部状态栏。上下文与当前模型额度属于输入区，执行状态位于输入框上方，点击可进入工作面板。
- 设置是带返回导航的独立页面，使用 Kit Settings 的分类导航与搜索，包含外观与布局、账号与模型、Agent 资源和快捷键。Shell 负责主题切换，文字大小、对话宽度与侧栏宽度保存到 GUI 偏好文件。账号页先列连接，点击添加后再选择登录方式或填写 API 表单。
- 恢复历史会连续读取已有分页，不等待实时轮询间隔；读取期间仅显示恢复状态，不把历史工具数量当成正在运行的进度。折叠工具内容在展开时解析 Markdown。

- 左侧列出当前主机上所有有会话的目录，展开目录选择会话；使用 Kit Tree 提供虚拟列表和方向键导航，Enter 打开会话。跨目录恢复会同步切换工作目录，保留当前窗口内的文字和附件草稿。
- “打开项目…”或 Ctrl+O 调用系统目录选择器；取消不改变当前会话。SSH/WSL 在命令面板的“打开远程项目…”中配置。远程目录列表仅属于当前连接主机。
- 项目移动或重命名后，打开新目录会按与 TUI 相同的目录身份找回历史。若从侧栏打开的旧目录已不存在，会提示选择迁移后的目录，先通过服务端重新关联原项目，再恢复原会话；跨文件系统移动或复制后删除旧目录也适用。取消保留当前会话和草稿；目标目录已有其他项目的会话时会拒绝合并。普通打开目录不会自动重新关联项目。
- 工作区错误统一进入通知，点击查看完整详情；输入区保留详情入口，未打开项目时位于任务标题栏。项目打开失败可在详情弹窗内重试。空会话隐藏不适用的变更、计划操作。
- 输入区模型菜单先列供应商／连接及模型数量，第二层只显示该连接下的模型名称；当前连接与当前模型都有勾选标记，提交时保留完整连接标识。
- 对话右侧提供按用户提问分轮的圆点定位栏：点击跳转、悬停预览，实心点跟随正文当前轮次；定位栏可独立滚动，底部显示轮次并可回到最新消息。聚焦定位栏后，↑/↓ 切换轮次，Home 到首轮，End 回到最新。
- 项目侧栏、任务标签、对话、可收起的工作面板。新任务的输入框位于页面中央，已有任务的输入框固定在对话底部。正文和输入框默认随任务区伸展，设置中可切换居中窄栏；侧栏与工作面板可拖动调整。
- 同时打开多个任务，切换只改变可见视图，后台运行、输入草稿、附件、滚动位置和面板选择继续保留。标签显示运行、待处理和未读状态；运行中任务不能关闭视图，停止需要显式操作。
- 重启后恢复打开的任务、标签顺序、当前标签和各任务的未发送文字、图片附件。没有发送过的新任务恢复为草稿，不尝试加载不存在的服务端会话。显式指定 `--workspace` 或 `--resume` 时仍打开指定目标。粘贴图片保留独立副本；普通附件引用原文件路径。
- 任务与草稿保存在 `$XDG_STATE_HOME/orbit/gui-tasks.json`（默认 `~/.local/state/orbit/gui-tasks.json`），图片副本位于旁边的 `gui-tasks.assets`。指定 `--preferences /path/gui.json` 时使用 `/path/gui.tasks.json` 与 `/path/gui.tasks.assets`。单个后台写入器原子替换快照，正常退出等待最后一次保存；保存失败会显示通知。服务程序、配置或数据库覆盖参数不同的启动不会恢复其他数据源的任务。
- 工作面板按执行、计划、变更、Agent 组织：工具详情来自事件记录；计划沿用服务端版本与审批；文件列表来自已记录的修改，可直接查看文件差异；子 Agent 从实际委派结果发现，可查看持续更新的执行记录或定向发送消息。
- 后台任务错误保留在所属任务，标签提示待处理，不在其他任务中弹出错误通知。返回该任务后可通过输入区打开详情。
- 原生 Markdown、代码、表格、行内与独立公式；公式按实际笔画范围裁边，保留文本基线。`MessageScroller` 管理虚拟列表、滚动和回到最新消息，工具与思考记录按需展开。
- 技术表格按实际文字测量列宽，单元格内不拆开中文短词或代码标识；宽度不足时横向滚动，正文仍正常换行。对话、工作面板报告和计划预览使用同一排版规则。
- 选择、粘贴、拖入和预览图片、移除附件；消息图片通过服务读取，支持远程工作区。多行输入、模型、推理强度、执行/计划与审批模式切换、发送、停止及运行中补充文字。收到服务确认后才清空已发送的草稿；普通发送失败可用相同请求 ID 重试。
- 工具审批使用 Dialog 展示完整操作证据，可允许一次、记住并允许或拒绝；计划审批使用独立表单。Agent 问答自动显示在输入区：编号选项、自由回复、跳过与发送；多题逐题作答，上一题与设置往返保留填写内容，收起后可重新展开。Enter 提交当前题，Shift+Enter 换行；失败保留答案供重试。关闭窗口只断开客户端，不取消服务端任务。
- 打开会话后自动请求额度，每 60 秒再查询一次；输入区显示当前模型所属额度池的两个额度条与重置倒计时，点击打开详情，可手动刷新。摘要复用 TUI 的共享额度选择逻辑；详情只显示账号共享额度，不列出单模型计数。Antigravity 的 Gemini 与 Claude / GPT 分别展示周、5h 窗口，没有共享额度数据时明确显示缺失，不用单模型计数代替。更新时间每秒重绘，刷新失败保留上次结果。
- 明暗主题、文字大小、对话宽度与侧栏宽度保存在 `$XDG_CONFIG_HOME/orbit/gui.json`，未设置 XDG 时使用 `~/.config/orbit/gui.json`。可用 `--preferences` 指定独立文件。

标题栏菜单或 `Ctrl+Shift+P` 打开可搜索的命令面板。以下操作直接调用现有客户端协议：

| 功能 | 入口 |
| --- | --- |
| 本地、SSH、WSL 项目 | 侧栏「打开项目」，或 `--ssh HOST` / `--wsl DISTRO`；SSH 密码和确认使用原生弹窗 |
| 文件变更、差异、文件读取、补丁导出、撤销 | 工作面板「变更」及命令面板；报告在右侧文档面板展示，可复制、保存；窄窗口使用完整文档视图 |
| 计划、Agent 提问、审批 | 工作面板「计划」、输入框上方的待办入口及命令面板 |
| 旁路提问、继续提问、停止、历史 | 命令面板；回答持续更新 |
| 子 Agent 查看与后续消息 | 工作面板「Agent」及命令面板；查看器持续订阅增量更新，关闭后停止订阅 |
| 项目任务、状态与释放；记忆添加、搜索与停用 | 命令面板 |
| 技能加载、刷新，MCP，插件来源与启停 | 设置中的「Agent 资源」及输入区命令搜索 |
| 上下文、压缩、状态、诊断 | 命令面板与输入区；压缩时显示实际阶段 |
| 会话重命名、删除、历史、压缩记录、存储；项目关联 | 命令面板，删除需要填写会话 ID |
| OAuth 与 API 连接 | 「账号与模型连接」；支持 Antigravity、ChatGPT、Gemini、Claude，以及 OpenAI Chat/Responses、Anthropic Messages API；保存后执行「刷新模型」 |

在输入框键入 `/`，命令补全列表会出现在输入框上方，继续输入即可筛选，方向键选择、Enter 确认、Esc 收起。列表和全局命令面板共用同一分发入口，支持中文名称及英文关键词搜索。

命令后的参数也在同一个列表中补全：模型连接与模型、技能、推理强度、工作模式、审批模式、任务和记忆子命令。`/cd` 的目录、`/diff` 与 `/undo` 的文件、`/resume` 的会话以及任务／记忆编号从当前工作区读取；远程工作区不会读取桌面本机的目录。Tab 或单击填入候选，有后续参数时继续提示；完整参数填入后 Enter 执行。目录以 Tab 继续展开，Enter 使用已补全的目录。Esc 收起后，迟到的响应不会重新打开列表。

输入框也支持 `/usage`、`/changes`、`/diff`、`/status`、`/context`、`/compact`、`/skills`、`/mcp`、`/diagnostics`、`/plan`、`/plugins`、`/tasks`、`/memory`、`/btw` 等已有入口。未知命令显示本地错误，不发送给模型。

| 操作 | 快捷键 |
| --- | --- |
| 发送 | Enter |
| 换行 | Shift+Enter |
| 新会话 | Ctrl+N |
| 下一个／上一个已打开任务 | Ctrl+Tab / Ctrl+Shift+Tab |
| 展开／收起工作面板 | Ctrl+Shift+E |
| 聚焦输入框 | Ctrl+L |
| 展开／收起侧栏 | Ctrl+B |
| 设置 | Ctrl+, |
| 命令面板 | Ctrl+Shift+P |
| 文件变更 | Ctrl+Shift+D |
| 搜索会话 | Ctrl+P |
| 关闭弹窗 | Esc |

草稿目前仅在窗口存活期间保留。OAuth 使用本机浏览器和回调端口；未提供手动回调地址输入。GUI 不包含 CLI 的打包、后台服务进程管理等维护命令。真实 OAuth 登录、SSH/WSL 连接需要在对应环境验收，隔离测试不替代这些验证。

## 运行与界面的边界

```text
Workbench（窗口与任务选择）
  └─ Workspace × N（一个任务一个长期 Entity）
       ├─ Backend（独立连接、事件游标、轮询与取消）
       │    └─ orbit-client → 工作区服务 → Agent / 工具 / 审批 / 存储
       ├─ Execution（只读事件投影：阶段、工具、修改记录、子 Agent）
       ├─ Conversation（消息 Entity 与滚动状态）
       └─ 输入草稿、附件、计划、问题、工作面板
```

`WorkspaceEvent` 只表达打开、激活、关闭和状态变更意图。Workbench 保留任务 Entity，不通过重建会话实现切换；项目目录列表只在相同主机、配置与数据源之间共享。会话 ID 相同但连接到不同 SSH/WSL 主机或数据库的任务不能相互替代。迁移后的新目录仍经过服务验证。

服务继续拥有执行、持久化、计划版本和权限决策，GUI 不另建 Agent 循环或数据库。`Execution` 消费协议事件：工具开始不代表文件已修改，父任务结束不代表后台子任务结束，压缩也不是等待模型回复。历史重放使用同一投影；新的用户轮次清空该轮工具列表，保留会话修改和委派记录。

设计研究参考了 [Gupi 的会话宿主与界面边界](https://github.com/suxiaoshao/gupi)、[Ghostex 的会话工作区](https://github.com/maddada/Ghostex)、[Farcaster 的运行控制](https://github.com/behzade/farcaster)、[Wake 的历史目录组织](https://github.com/iAmCorey/Wake)、[Vibex 的运行与 UI 边界](https://github.com/vibex-ai/vibex)，以及 [T3 Code 的 orchestration v2 设计文档](https://github.com/pingdotgg/t3code/tree/main/docs/orchestration-v2)。后者是目标架构文档；Wake 主要用于历史浏览。这里借鉴职责和信息组织，未复制这些项目的界面代码、品牌或素材。

## GPUI Kit 与 Shell 约定

实现遵循 `gpui-kit`、`gpui-kit-design-guides` skills，以及所依赖 GPUI Kit 的 AGENTS.md、Coding Guides 和 Design Guides：

- `gpui_kit::init` + `gpui_kit::open_window` 负责宿主与 overlay；不另建 Root。
- Button、Tree、TabBar、Textarea、Attachment、Message、MessageScroller、Progress、Dialog、Sheet 和 Resizable 均复用 Component。
- 输入、消息和滚动状态由长期存在的 Entity 持有；任务和订阅跟随窗口。流式事件只更新对应消息的文本状态。
- 布局采用 rem 与组件尺寸，颜色来自主题 token。原生窗口边界和用户选择的字体字号使用像素，其余布局使用 rem。
- Tokio 处理连接与文件读取，通过有界事件队列回到 GPUI。额度使用独立连接，避免阻塞消息轮询与取消。
- `Theme::change` 同步 Component 与 Base/Shell；偏好文件由一个后台写入者串行、原子替换。

默认 Shell 在 [appearance/main.js](appearance/main.js)，也可指定 `--appearance /path/to/directory`。调试构建会监听脚本变化；运行时生成的 `gpui-kit.d.ts` 与模块声明不提交。Host Module `orbit-appearance` 只暴露主题读写，不向脚本开放模型凭据、文件系统或工具执行能力。

明暗配色集中在 [appearance/theme.json](appearance/theme.json)，由宿主加载到 Component 主题，Shell 外观面板与原生控件共享相同 token。

GPUI Kit、Shell 和 Component Shell 锁定到同一提交 `1ccf011`。Shell 依赖它的 rquickjs 兼容层，因此 workspace 包含相应 patch；LLRT 的 RSA/PKCS1 预发布依赖采用该版本 Kit 的兼容锁定组合。Kit 和 Orbit 的 tree-sitter 都带 native `links`，workspace 统一为 0.26.13，仓库解析回归需一起验证。构建请保留 `--locked`。

## 验证

```sh
cargo test -p orbit-gui --features test-support --locked
cargo test -p orbit-tools --lib repository:: --locked
cargo xtask check
cargo clippy -p orbit-gui --features test-support --all-targets --locked -- -D warnings
```

GUI 协议测试完全使用 Rust。[tests/transport.rs](tests/transport.rs) 启动 [tests/support/server.rs](tests/support/server.rs) 编译得到的隔离服务，不连接真实模型、不读取用户凭据。测试覆盖空会话重复恢复、发送失败重试、额度、审批、停止、无消息增量时的计划与问答更新、文件解码、图片失败、旁路回答、子 Agent 增量和运行中补充输入；公式几何另有 Rust 单元测试。测试服务只在启用 `test-support` 时构建。

独立原生 UI 验收可运行：

```sh
cargo build -p orbit-gui --features test-support --locked
cargo run -p orbit-gui --locked -- \
  --server "$PWD/target/debug/orbit-gui-fixture" \
  --config /tmp/orbit-gui-fixture.toml --auth-dir /tmp/orbit-gui-fixture-auth \
  --database /tmp/orbit-gui-fixture.db \
  --model fixture::local --resume demo \
  --preferences /tmp/orbit-gui-preview.json
```

发送 `fail` 会失败一次，重试成功；`approve` 出现审批入口，`question` 出现 Agent 表单，`math` 展示公式。`workbench` 模拟约十秒的工具执行、委派和文件修改，可在执行期间 Ctrl+N 新建任务、输入草稿，再用 Ctrl+Tab 返回检查原任务继续运行，随后验证工作面板的计划审阅、文件差异、子 Agent 查看与发消息。

检查 Enter 发送、Shift+Enter 换行、Tab/Shift+Tab、Esc、命令搜索、长消息滚动、800×560 窄窗口、主题切换与重新打开后的偏好恢复。fixture 中的数字和文件仅用于测试布局，实际数据取自服务端。Rust 回归覆盖独立连接的任务与审批隔离，以及工具执行、后台委派、完成、失败、压缩和文件修改的事件投影。
