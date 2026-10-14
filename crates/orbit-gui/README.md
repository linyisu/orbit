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

- 项目支持置顶和拖动排序；标签保留各自草稿与运行状态，关闭当前标签后优先选择左邻。
- 新会话在输入框上方选择目录及本地、SSH、WSL；切换目标保留草稿，右上角只保留工作面板入口。
- 输入区支持图片、历史输入与暂存消息。编辑暂存消息会撤回到输入框；响应时空草稿显示停止，有内容时显示发送。
- 用户消息保留原文并支持悬停复制，AI 回答支持全文复制及本地、网络、内嵌图片；缺失历史图片不阻止继续会话。
- 工作面板提供执行、计划、变更和 Agent；执行区内部滚轮不带动外层会话。审批和变更的统一差异只显示一列行号。
- 当前会话的工具审批和计划待审自动展示；关闭后保留手动入口，后台请求在切回时显示。操作弹窗统一沿用搜索任务的宽度、位置和间距。
- MCP 保存后可“应用到当前会话”，新工具从下一次发送开始使用。

常用快捷键：Ctrl+O 打开项目，Ctrl+W 关闭标签，Ctrl+. 停止响应；设置中可改绑。其他入口见 [功能对照](../../FRONTEND_PARITY.md)。

## 源码与外观

[workspace/](src/workspace/) 管理会话视图与设置，[conversation/](src/conversation/) 渲染消息，[backend.rs](src/backend.rs) 处理连接与请求。

GPUI Kit 使用固定的 fork 提交，补丁来源见 [依赖补丁](../../patches/README.md)。

Shell 入口为 [appearance/main.js](appearance/main.js)，主题位于 [appearance/theme.json](appearance/theme.json)。`--appearance DIR` 覆盖外观目录，调试构建监听脚本变化；外观脚本仅暴露主题接口。

## 验证

```sh
cargo test -p orbit-gui --features test-support --locked
```

[传输测试](tests/transport.rs) 使用隔离的 [fixture 服务](tests/support/server.rs)，不连接真实模型。原生验收使用 Xvfb、临时 HOME、配置、数据库和工作区，避免操作用户桌面。
