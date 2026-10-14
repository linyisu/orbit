# 依赖补丁

GPUI Kit 修复已发布到 [fork 补丁分支](https://github.com/linyisu/gpui-component/tree/codex/orbit-framework-fixes)，
Cargo 固定使用提交 `f2e0656f9883dd203304014fe9e4568e18a3e51b`，无需复制框架源码。

`gpui-kit.patch` 保存该提交相对上游 `1ccf011620da24d633f7cd6d3c11ca2e97798dcf`
的完整补丁，包含选择、滚动、图片、表格及差异预览修复，沿用 Apache-2.0。

`vendor` 仅保留 GPUI WGPU 字体回退和 RaTeX 日志补丁；说明见各目录的 README。
