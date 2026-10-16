# orbit-gui

实验性 GPUI Kit 桌面客户端，通过 `orbit-client` 使用工作区服务。提供项目与标签、对话、工作面板，以及账号、模型、MCP 和快捷键设置。

## 启动

先按 [根 README](../../README.md#快速开始) 准备工具链与配置。Ubuntu 的额外依赖：

```sh
sudo apt install build-essential clang pkg-config libfontconfig-dev libwayland-dev \
  libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev libasound2-dev libvulkan-dev
cargo run -p orbit-gui --locked -- --config config.toml
```

支持 `--workspace`、`--model`、`--resume`、`--ssh` 与 `--wsl`，其他参数见 `--help`。GUI 内嵌静态服务，暂未加入便携打包。

## 使用

- 回答分为思考、工具列表和正文；思考片段结束、工具批次结束后自动收起，手动选择优先。
- 输入框上方显示当前状态及最新进度，点击打开过程记录。工具行显示名称、目的和状态，
  点击打开详情：命令分 stdout/stderr，文件变更显示已提交差异，参数及原始结果单独保留。

- 项目支持置顶和拖动排序；启动从共享客户端恢复标签、选中项和草稿，关闭当前标签后优先选择左邻。
- 侧栏顶部切换本地与远程环境；远程下拉列出 SSH 和 WSL，支持添加连接并记住最近选择；目录在新任务中选择。快捷入口为新任务、搜索、MCP、Skill 和插件。
- 远程连接的主机和目录使用可输入下拉；目录提供 `~/`、`../` 与子目录，支持键盘选择。
- 新会话在输入框上方选择目录及本地、SSH、WSL；切换目标保留草稿，右上角只保留工作面板入口。
- 输入区支持文件附件、图片、历史输入与暂存消息；粘贴文件自动生成附件卡片，达到 4096 字节或超过 32 行的文本转成文本附件。编辑暂存消息会撤回到输入框；响应时空草稿显示停止，有内容时显示发送。
- 消息内图片在前、文件在后；发送前后的文件卡片均可点击查看只读文本并复制。已发送附件通过所属会话服务读取，SSH/WSL 路径不由本机解释。
- 用户消息保留原文并支持悬停复制，AI 回答支持全文复制及本地、网络、内嵌图片；缺失历史图片不阻止继续会话。未指定语言的代码块显示 `text` 标签。
- 工作面板提供执行、计划、变更和 Agent；执行区内部滚轮不带动外层会话。审批和变更的统一差异只显示一列行号。
- 当前会话的工具审批和计划待审自动展示；关闭后保留手动入口，后台请求在切回时显示。操作弹窗统一沿用搜索任务的宽度、位置和间距。
- 技能与插件市场在全局设置中管理，无需连接会话；技能支持创建、编辑和完整包导入。
- “应用到当前会话”只改变所选会话；技能与插件可从会话移除，全局定义保持不变。应用快照可随会话恢复，市场更新不会自动覆盖。
- 插件目前应用包中的技能；详情保留 Hook、MCP、命令和 Agent 声明，这些组件不会自动运行。
- MCP 保存后可“应用到当前会话”，新工具从下一次发送开始使用。

常用快捷键：Ctrl+O 打开项目，Ctrl+W 关闭标签，Ctrl+. 停止响应；设置中可改绑。其他入口见 [功能对照](../../FRONTEND_PARITY.md)。

## 源码与外观

[workspace/](src/workspace/) 管理会话视图与设置，[conversation/](src/conversation/) 渲染消息，[backend.rs](src/backend.rs) 将窗口事件接入共享客户端。认证、配置校验、会话轮询与附件传输由对应共享 crate 提供，前端只保留交互和视图状态。

GPUI Kit 使用固定的 fork 提交，补丁来源见 [依赖补丁](../../patches/README.md)。

Shell 入口为 [appearance/main.js](appearance/main.js)，主题位于 [appearance/theme.json](appearance/theme.json)。`--appearance DIR` 覆盖外观目录，调试构建监听脚本变化；外观脚本仅暴露主题接口。

## 验证

```sh
cargo test -p orbit-gui --features test-support --locked
```

[传输测试](tests/transport.rs) 使用隔离的 [fixture 服务](tests/support/server.rs)，不连接真实模型。原生验收使用 Xvfb、临时 HOME、配置、数据库和工作区，避免操作用户桌面。
