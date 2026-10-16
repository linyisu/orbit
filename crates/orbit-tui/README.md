# orbit-tui

Ratatui 终端客户端，可执行程序名为 `orbit`。会话执行由工作区服务负责，界面管理输入、标签、选区、排版与媒体缓存。

## 启动

按 [根 README](../../README.md#快速开始) 准备工具链和配置：

```sh
cargo run --release -p orbit-tui --locked
```

图标需要 Nerd Font。图片使用 Kitty、Sixel 或 iTerm2 协议，不支持时回退为字符预览；公式回退为 TeX。

## 常用操作

| 操作 | 入口 |
| --- | --- |
| 账号与模型 | `/connect`、`/model`、`/model refresh` |
| 会话与标签 | `/new`、`/resume`、`/close`、`/restore` |
| 计划与审批 | `/plan`、`/plan view`、`/build`、`/permissions` |
| 文件与变更 | `/file`、`/review`、`/changes`、`/diff`、`/undo` |
| 任务与扩展 | `/tasks`、`/queue`、`/skills`、`/mcp`、`/plugins` |
| 快捷键与历史 | `/keys`；Ctrl+Up / Ctrl+Down 浏览输入历史 |

`/skills`、`/plugins` 管理本机全局库；显式应用只影响当前会话，SSH/WSL 接收同一包的快照。全局保存不写入项目目录。

标签按标题收缩并限制最大宽度，长标题悬停时滚动。消息定位编号位于输入区右侧，到达最新消息后隐藏返回底部箭头。

## 工作区连接

启动时自动恢复与 GUI 共用的标签、选中项、未发送文字和附件；`/restore` 保留为手动补充入口。
恢复不会发送草稿或重跑中断任务。

`/ssh HOST PATH`、`/wsl DISTRO PATH` 打开远端标签，`/local` 新建本地标签。省略连接参数会打开向导；切换或关闭标签不会停止后台任务，Ctrl+C 取消当前运行。

连接、部署和恢复规则见 [orbit-client](../orbit-client/README.md)，GUI 对应入口见 [功能对照](../../FRONTEND_PARITY.md)。

## 源码与验证

[tui/workspace_client/](src/tui/workspace_client/) 绑定客户端事件和交互状态，[tui/render/](src/tui/render/) 绘制界面，[tui/markdown.rs](src/tui/markdown.rs) 和 [tui/media/](src/tui/media/) 负责正文与媒体。

```sh
cargo test -p orbit-tui --locked
```
