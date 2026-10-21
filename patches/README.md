# 依赖补丁

依赖补丁维护固定来源、许可和功能约束。Cargo 使用确定提交，补丁文件提供对应差异；
第三方源码保持上游格式。

## GPUI Kit

框架来源为 [fork 的 orbit-rely 分支](https://github.com/linyisu/gpui-kit/tree/orbit-rely)，
Cargo 固定提交 `d7b441dbc0f6dac2cdea76d9df5eb44ffb13f4e4`。
`gpui-kit.patch` 记录提交 `7b1a4b8b86c71ca036fa941fd6565e6a05b58f8c`
相对上游 `9254c58028cebe8122b989c30e15f9139e0b2569` 的组件差异，采用 Apache-2.0 许可。

- MessageScroller 支持暂停跟随；手动展开保留阅读位置，回到底部后恢复跟随。
- 表格和行内代码使用 Unicode 换行边界。行内代码以独立对象参与断行，超宽代码按字素拆分。
- 列宽包含两侧内边距各自取整的像素预算；无语言代码块显示 `text`。
- 只读代码视图支持选择、复制和源码起始行号，隐藏插入光标。滚动条与文本区共用高度预算。
- SpeechWaveform 的 `live()` 模式在静音或音频中断 320 ms 后回到基线，默认模式保留历史波形。
- Debug 与发布构建均内嵌已注册图标，运行时不读取构建机器的源码目录。

组件脚本由 `gpui-shell::components` 提供，Orbit 启用所需的 QuickJS 与组件能力。
底层 GPUI 版本为 0.3.9。

## 平台渲染与字体

Windows 补丁随上述 fork 固定，来源和许可见 fork 的 `patches/gpui-pre-windows`；
根 Cargo patch 引用该来源。DPI 切换使用 Win32 外框矩形，以实际客户区尺寸更新渲染。

[GPUI WGPU 补丁](../vendor/gpui-pre-wgpu/README.orbit.md) 提供符号和 Emoji 字体回退，
并保留字体选择与预览的回归覆盖。RaTeX 日志补丁位于 `vendor/ratex-unicode-font`。

## 本机语音

[Qwen 增量补丁](../vendor/qwen-asr/ORBIT_PATCH.md) 说明推理库来源、平台适配及增量缓存契约，
完整差异见 `qwen-asr.patch`。

## Android

NSH 的固定提交、补丁应用与构建方式见 [Android 客户端](../mobile/README.md#构建与验证)。
`mobile/patches` 提供 Rust 运行时、Activity 桥接、内嵌图片和原生输入适配。
