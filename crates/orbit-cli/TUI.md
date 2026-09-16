# 流式输出与滚动

滚轮与键盘事件更新状态后直接请求一帧，不等待模型输出的刷新时钟。`frames.rs` 合并同一帧前的多次请求，并将常规绘制频率限制在约 120 FPS；模型输出按 33ms 合并刷新。逾期刷新跳过，不补画已经过时的帧。框选仍直接使用冻结的可见缓冲区，保留原有复制行为。

`transcript.rs` 按消息和逻辑行缓存格式化文本、换行高度及累计高度。滚动通过二分定位可见消息和行，仅将可见逻辑行交给 Ratatui 绘制。流式追加只重算未完成的末行和新行，代码块状态跨分片保存。宽度变化或展开/折叠会重建布局；工具结果变化会从受影响的消息更新。中文、组合字符、emoji 和单行换行仍由 Ratatui 处理，没有另写一套字符宽度算法。

用户向上滚动后，新输出增加底部距离，保持当前可见位置；滚回底部后恢复跟随。总滚动位置用 `usize`，不再把整段对话截在 65535 行。

## 对照的官方实现

- [Codex FrameRequester](https://github.com/openai/codex/blob/main/codex-rs/tui/src/tui/frame_requester.rs)：合并帧请求、限制最高绘制频率。Orbit 用主事件循环内的截止时间实现相同的调度思路，未引入额外 actor。
- [Codex transcript overlay](https://github.com/openai/codex/blob/main/codex-rs/tui/src/pager_overlay.rs)：历史块按宽度缓存高度；流式尾部按宽度和内容修订缓存。Orbit 进一步按逻辑行保存累计高度，以便直接定位视口。
- [OpenTUI ScrollBox](https://github.com/anomalyco/opentui/blob/main/packages/core/src/renderables/ScrollBox.ts)：可见区域裁剪，以及手动滚动和 sticky bottom 的状态区分。
- [Ratatui Paragraph](https://docs.rs/ratatui/latest/ratatui/widgets/struct.Paragraph.html)：滚动发生在换行之后。将整段历史交给一个 Paragraph 并不能避免历史重排，因此 Orbit 缓存行高并仅传入可见行。

这些是设计参考，不表示直接移植代码，也没有把不同项目的框架混用。Codex 主界面的终端原生 scrollback 和 Orbit 的全屏界面不同；这里参考的是其独立 transcript overlay。

## 验证

`cargo test -p orbit-cli --offline` 覆盖流式追加时视口不跳、回到底部恢复跟随、分片代码围栏、中文和组合字符换行与 Ratatui 原始渲染的一致性、一万行历史的缓存复用、帧请求合并，以及现有框选/审批滚动回归。

缓存主要消除了历史重复排版的成本。首次加载和改变终端宽度仍需计算历史布局；单个特别长且不换行的源文本行仍需按该行长度处理。
