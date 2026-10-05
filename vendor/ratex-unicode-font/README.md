# ratex-unicode-font 补丁

源码来自 [RaTeX](https://github.com/erweixin/RaTeX/tree/08cae05377938391117913ca4f278e6a3ffb6a8a/crates/ratex-unicode-font) 在 crates.io 发布的 `ratex-unicode-font 0.1.14`，对应提交 `08cae05377938391117913ca4f278e6a3ffb6a8a`，沿用上游 MIT 许可。

上游字体发现函数直接调用 `eprintln!`，中文公式首次加载字体时会绕过 TUI 绘制，覆盖终端光标所在的输入区域。此处仅将这些诊断及上游测试的输出改为 `log::debug!`，增加 `log` 依赖；字体发现、缓存、TTC 索引及 Emoji 行为保持上游实现。

根目录通过 `[patch.crates-io]` 应用补丁，其他 RaTeX 模块仍使用发布版。升级 RaTeX 时需检查上游是否已提供默认静默的日志方式；可用后移除此补丁，并保留 `tui/media/tests.rs` 中独立进程的终端输出回归测试。
