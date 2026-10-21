# Orbit 字体回退补丁

## 来源与许可

来源为 crates.io `gpui-pre-wgpu` 0.3.9，基于
[Zed `93d74566`](https://github.com/zed-industries/zed/tree/93d745668805029476e6d7fbb19652116fed51de/crates/gpui_wgpu)。
许可见 [Apache-2.0](LICENSE-APACHE)。

## 字体加载契约

`load_family` 接受不含 ASCII `m` 的字体，字体选择和预览后保留 Emoji 与符号回退。
应用通过 `Font.fallbacks` 指定回退字体；文字排版及原生字素回退使用上游实现。

## 验证

[font_fallback.rs](tests/font_fallback.rs) 直接验证文字系统，无需窗口。
内嵌 Zed 与 Noto 字体样本保留各自许可；缺少所需系统字体的用例显式跳过。
