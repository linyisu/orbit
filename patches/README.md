# 依赖补丁

GPUI Kit 修复已发布到 [fork 补丁分支](https://github.com/linyisu/gpui-kit/tree/orbit-rely)，
Cargo 固定使用提交 `5f9de8a41cc2762866d076057ef2452fdbae1876`，无需复制框架源码。

`gpui-kit.patch` 保存该提交相对上游 `9254c58028cebe8122b989c30e15f9139e0b2569`
的完整补丁，包含选择、滚动、图片、表格及差异预览修复，沿用 Apache-2.0。
MessageScroller 提供暂停自动跟随的入口，手动展开内容时保留阅读位置，显式回到底部后恢复。
表格与行内代码使用 Unicode 换行边界，避免闭括号和标点被单独留在行首。
列宽包含左右内边距各自取整的像素预算，避免字体缩放后短标签仍被拆行；未指定语言的代码块标签显示 `text`。

`vendor` 保留 GPUI WGPU 字体回退、RaTeX 日志与 Qwen 增量识别补丁；说明见各目录。

只读代码视图隐藏插入光标但保留选择与复制；编辑器支持源码起始行号，滚动条与文本区使用一致的高度预算，组件内边距不会移动滚动条。

`orbit-rely` 是 Orbit 框架补丁的统一维护分支，后续补丁继续在此维护，Cargo 仍固定具体提交。
已合并上游 `main` 的上述提交；此前两条 Orbit 专用 Codex 分支已合入并删除，历史提交完整保留。
上游将组件脚本目录合入 `gpui-shell::components`；Orbit 仅启用所需的 QuickJS 与组件能力。
底层 GPUI 更新为 0.3.9，WGPU 补丁同步此版本并保留符号与 Emoji 字体加载回归。

SpeechWaveform 的可选 `live()` 模式在持续静音或音频中断 320 ms 后回到基线，保留默认历史波形行为。

[Qwen 增量补丁](../vendor/qwen-asr/ORBIT_PATCH.md) 固定原生库来源，边界修复见 `qwen-asr.patch`。
