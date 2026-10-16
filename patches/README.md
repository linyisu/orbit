# 依赖补丁

GPUI Kit 修复已发布到 [fork 补丁分支](https://github.com/linyisu/gpui-component/tree/codex/orbit-framework-fixes)，
Cargo 固定使用提交 `889c130bedf1acc254f894564e65355b941a84eb`，无需复制框架源码。

`gpui-kit.patch` 保存该提交相对上游 `1ccf011620da24d633f7cd6d3c11ca2e97798dcf`
的完整补丁，包含选择、滚动、图片、表格及差异预览修复，沿用 Apache-2.0。
MessageScroller 提供暂停自动跟随的入口，手动展开内容时保留阅读位置，显式回到底部后恢复。
表格与行内代码使用 Unicode 换行边界，避免闭括号和标点被单独留在行首。
列宽包含左右内边距各自取整的像素预算，避免字体缩放后短标签仍被拆行；未指定语言的代码块标签显示 `text`。

`vendor` 仅保留 GPUI WGPU 字体回退和 RaTeX 日志补丁；说明见各目录的 README。
