# ratex-unicode-font 补丁

来源：[RaTeX `08cae053`](https://github.com/erweixin/RaTeX/tree/08cae05377938391117913ca4f278e6a3ffb6a8a/crates/ratex-unicode-font)，crates.io 版本 `0.1.14`，沿用 MIT 许可。

字体发现与测试的诊断输出改用 `log::debug!`，避免覆盖 TUI 输入区；字体发现、缓存和 Emoji 行为保持不变。

通过根 `[patch.crates-io]` 应用。上游默认静默后移除补丁，保留终端输出回归测试。
