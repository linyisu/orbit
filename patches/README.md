# 依赖补丁

GPUI Kit 修复已发布到 [fork 补丁分支](https://github.com/linyisu/gpui-kit/tree/orbit-rely)，
Cargo 固定使用提交 `d7b441dbc0f6dac2cdea76d9df5eb44ffb13f4e4`，无需复制框架源码。

`gpui-kit.patch` 保存此前组件补丁提交 `7b1a4b8b86c71ca036fa941fd6565e6a05b58f8c`
相对上游 `9254c58028cebe8122b989c30e15f9139e0b2569`
的完整补丁，包含选择、滚动、图片、表格及差异预览修复，沿用 Apache-2.0。
MessageScroller 提供暂停自动跟随的入口，手动展开内容时保留阅读位置，显式回到底部后恢复。
表格与行内代码使用 Unicode 换行边界，避免闭括号和标点被单独留在行首。
行内代码以独立对象参与正文的断行判定，代码内部的点号等符号不影响相邻正文；
窄窗口下短代码保持完整，连续代码可正常换行，超宽代码仍可按原有字素边界拆分。
列宽包含左右内边距各自取整的像素预算，避免字体缩放后短标签仍被拆行；未指定语言的代码块标签显示 `text`。

`vendor` 保留 GPUI WGPU 字体回退、RaTeX 日志与 Qwen 增量识别补丁；说明见各目录。

只读代码视图隐藏插入光标但保留选择与复制；编辑器支持源码起始行号，滚动条与文本区使用一致的高度预算，组件内边距不会移动滚动条。

`orbit-rely` 是 Orbit 框架补丁的统一维护分支，后续补丁继续在此维护，Cargo 仍固定具体提交。
已合并上游 `main` 的上述提交；此前两条 Orbit 专用 Codex 分支已合入并删除，历史提交完整保留。
上游将组件脚本目录合入 `gpui-shell::components`；Orbit 仅启用所需的 QuickJS 与组件能力。
底层 GPUI 更新为 0.3.9，WGPU 补丁同步此版本并保留符号与 Emoji 字体加载回归。
Windows 原生平台补丁也固定到上述 fork 提交，源码、发布版来源和许可位于 fork 的
`patches/gpui-pre-windows`；Orbit 通过根 Cargo patch 使用，不复制到本仓库 vendor。
DPI 切换使用 Win32 提供的外框矩形，并以实际客户区尺寸更新渲染，避免最大化时重复扣除系统边框。

SpeechWaveform 的可选 `live()` 模式在持续静音或音频中断 320 ms 后回到基线，保留默认历史波形行为。

原生默认图标资源在 Debug 中也使用静态内嵌数据，安装后不读取构建机器的源码目录；
覆盖侧栏、工具栏与 Windows 窗口按钮。未注册的完整图标目录仍不会被默认打包。

[Qwen 增量补丁](../vendor/qwen-asr/ORBIT_PATCH.md) 固定原生库来源，边界修复见 `qwen-asr.patch`。
