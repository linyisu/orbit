# 流式输出与滚动

滚轮、键盘和窗口尺寸事件更新状态后直接请求一帧，不等待模型输出的刷新时钟。`frames.rs` 合并同一帧前的多次请求，并将常规绘制频率限制在约 120 FPS；模型输出按 33ms 合并刷新。终端使用固定视口；尺寸变化时重建双缓冲区并以普通单元格更新覆盖旧画面，绕过 Ratatui 全屏视口自动缩放时的 `Clear(All)`。实际绘制使用终端同步更新协议提交完整帧，不向支持该协议的终端暴露中间状态。逾期刷新跳过，不补画已经过时的帧。框选仍直接使用冻结的可见缓冲区，保留原有复制行为。

`transcript.rs` 按消息和逻辑行缓存格式化文本、换行高度及累计高度。滚动通过二分定位可见消息和行，仅将可见物理行交给 Ratatui 绘制。流式追加只重算未完成的末行和新行，代码块状态跨分片保存。宽度变化或展开/折叠会重建布局；工具结果变化会从受影响的消息更新。正文按照 Unicode UAX #14 的合法断行点排版，中文可逐字换行，英文优先保留完整单词；组合字符和 emoji 保持在同一字素簇内。

用户向上滚动后，新输出增加底部距离，保持当前可见位置；滚回底部后恢复跟随。窗口宽度变化时，视口以当前消息、逻辑行和源文本字节位置为锚点，在新布局中回到同一段内容。总滚动位置用 `usize`，不再把整段对话截在 65535 行。

## 对照的官方实现

- [Codex FrameRequester](https://github.com/openai/codex/blob/main/codex-rs/tui/src/tui/frame_requester.rs)：合并帧请求、限制最高绘制频率。Orbit 用主事件循环内的截止时间实现相同的调度思路，未引入额外 actor。
- [Codex transcript overlay](https://github.com/openai/codex/blob/main/codex-rs/tui/src/pager_overlay.rs)：历史块按宽度缓存高度；流式尾部按宽度和内容修订缓存。Orbit 进一步按逻辑行保存累计高度，以便直接定位视口。
- [OpenTUI ScrollBox](https://github.com/anomalyco/opentui/blob/main/packages/core/src/renderables/ScrollBox.ts)：可见区域裁剪，以及手动滚动和 sticky bottom 的状态区分。
- [Ratatui Paragraph](https://docs.rs/ratatui/latest/ratatui/widgets/struct.Paragraph.html)：滚动发生在换行之后。将整段历史交给一个 Paragraph 并不能避免历史重排，因此 Orbit 缓存行高并仅传入可见行。

这些是设计参考，不表示直接移植代码，也没有把不同项目的框架混用。Codex 主界面的终端原生 scrollback 和 Orbit 的全屏界面不同；这里参考的是其独立 transcript overlay。

## 验证

`cargo test -p orbit-cli --offline` 覆盖流式追加时视口不跳、回到底部恢复跟随、分片代码围栏、中文、英文和组合字符的断行、窗口缩放后的文本锚点、一万行历史的缓存复用、帧请求合并，以及现有框选/审批滚动回归。

缓存主要消除了历史重复排版的成本。首次加载和改变终端宽度仍需计算历史布局；Ratatui 的终端差分只提交可见区域中实际变化的单元格，调整宽度时不会把历史内容写回终端滚屏。

## 会话恢复与帮助

参考 [Claude Code 的 `/resume` 会话选择器](https://code.claude.com/docs/en/sessions)和 [OpenCode 的会话对话框](https://github.com/anomalyco/opencode/blob/dev/packages/tui/src/component/dialog-session-list.tsx)：

- `/resume` 打开当前项目、相同运行模式的历史会话列表，按最近更新排序。输入标题或 ID 筛选，↑↓ 或滚轮选择，Enter 恢复，Esc 返回；`/sessions` 是同一入口。
- `/resume <完整 ID>` 直接恢复。当前会话和无历史的空会话不列入选择器。运行中的任务必须先结束或取消，不能直接切换。
- 切换先校验项目和模式，再取得目标会话的独占锁并加载上下文，成功后才释放当前会话。不存在、跨项目、模式不匹配或已被其他进程占用时保留当前会话，不重跑旧工具。
- 恢复沿用当前 API 配置、模型和预算；审批模式回到 auto。历史记录与输入历史从目标会话重建，后续消息追加到原会话 ID。

`/` 直接显示可用命令；已移除 `/help` 命令，空输入时 `?` 仍可查看快捷键。新对话页只显示 Orbit ASCII 标志，底栏只显示模型和权限模式。

## 模型与思考强度

模型由配置中的每个供应商声明；Orbit 不请求远程模型目录。`/model` 的列表按供应商分组，`↑↓` 选模型，`←/→` 选该供应商声明的思考强度，Enter 同时应用。`/model 模型名` 只在当前供应商中查找，`/model 供应商::模型名` 可直接跨供应商切换。

在供应商下可选地写 `reasoning_efforts`，列表首项是启动和直接切换模型时的默认值，例如：

```toml
[providers.openai]
api_format = "openai_responses"
base_url = "https://api.openai.com/v1"
api_key = "replace-with-key"
models = ["gpt-5.6-sol", "gpt-6-astra"]
reasoning_efforts = ["low", "medium", "high"]
```

可用值固定为五档：`minimal`、`low`、`medium`、`high` 和 `xhigh`，但应只列出该供应商和模型实际接受的值。未写此项时，选择器会说明未配置，Orbit 不发送任何思考参数，避免给通用中转站增加未知字段。已配置时，Responses 使用 `reasoning.effort`，Chat Completions 使用 `reasoning_effort`；Anthropic Messages 使用 adaptive thinking 的 `thinking` 与 `output_config.effort`。模型或中转站拒绝某个显式配置的值时，Orbit 会展示服务端错误。

## 图片与文档

直接把 PNG、JPEG、GIF 或 WebP 文件拖进终端或粘贴它的路径，会将图片加入下一条消息；`Ctrl+V`（部分终端使用 `Alt+V`）还能直接读取系统剪贴板中的位图。输入框上方以 `[图片 N · 文件名]` 显示待发送附件。光标位于文字开头时按 ← 可进入图片，←→ 在图片间移动，Backspace/Delete 删除选中的图片；也可以点击图片后删除。只有图片而没有文字时也能直接发送。`/attach <路径>` 保留为兼容终端和远程环境的兜底入口，`/detach` 清空待发送图片。普通文本粘贴不会被当作附件。

附件复制到全局数据库旁的 `orbit.attachments` 目录并按内容哈希去重，不在用户项目中创建文件；会话历史保存附件引用而不是 Base64。底栏显示待发送图片数量。OpenAI Chat Completions、OpenAI Responses 和 Anthropic Messages 分别编码为各自的原生图片内容块。剪贴板读取基于 `arboard`，在桌面 X11、Wayland、Windows 和 macOS 上使用系统剪贴板；SSH 或不转发剪贴板的终端仍应使用拖入路径或 `/attach`。

模型也可以调用 `read_image` 打开工作区内的图片。因此“分析 `screenshots/error.png`”这类任务不要求用户先执行 `/attach`：工具通过受限工作区句柄读取和验证图片，复制到同一全局附件目录，并把图片作为原生视觉内容加入下一次模型请求。文件列表、`read_file` 和 shell 输出都不能代替 `read_image`。如果当前模型或中转站不接受图片内容块，请求会明确失败，不能靠 OCR 或像素统计假装已经看过图片。

每次发给模型最多保留最近四张历史图片；更早图片在请求副本中替换为带文件名的文字占位，持久化会话仍保留原引用。上下文估算为每张图片预留近似预算，防止大量截图绕过自动压缩。

模型通过 `read_document` 读取工作区内的 PDF、Word、PowerPoint、Excel、OpenDocument、RTF、EPUB 和 CSV。AnyDoc 在本地转为 Markdown，按字符分页，不产生项目内中间文件。扫描 PDF 需要 OCR，目前明确报错，不会静默上传用户文档。

模型 API 返回的思考内容实时完整显示，不再需要展开。工具结果默认预览两行；`Ctrl+O` 只展开或收起当前视口最下面的工具结果，不再批量展开整段历史。滚动到较早的调用后也可使用；再次选择另一条时收起原来展开的结果。

会话选择器、独立帮助和局部导航沿用主流 TUI 的交互方式，Orbit 的中文文案、分组和尺寸适配由本项目实现，并不声称逐像素复刻其他产品。

### 目录改名与会话归属

会话关联全局数据库中的项目 ID。文件系统提供完整目录身份时，同一文件系统内改名后，从新目录打开 TUI，`/resume` 仍能看到原会话。不会在项目或 `.git` 中创建身份标记。

跨盘移动等无法自动识别的情况，先关闭该项目 TUI，在新目录运行 `orbit projects list` 查找原项目 ID，再执行 `orbit projects link <项目ID>`。原目录必须已不存在，目标已有其他对话时拒绝合并。然后重新打开 TUI 使用 `/resume`。

旧数据库自动迁移；升级前就已移动的项目需要手动关联。平台支持和数据库位置见 [会话存储说明](../orbit-store/README.md)。
