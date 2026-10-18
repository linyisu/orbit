# Orbit text fallback patch

Source: crates.io `gpui-pre-wgpu` 0.3.9, based on
[Zed `93d74566`](https://github.com/zed-industries/zed/tree/93d745668805029476e6d7fbb19652116fed51de/crates/gpui_wgpu).
License: [Apache-2.0](LICENSE-APACHE).

`load_family` keeps fonts without ASCII `m`, preserving emoji and symbol
fallback after font selection or preview. Text and native grapheme fallback
remain unchanged; the application supplies `Font.fallbacks`.

[tests/font_fallback.rs](tests/font_fallback.rs) tests the text system without
a window. Bundled Zed and Noto font fixtures retain their licenses; cases
requiring unavailable system fonts explicitly skip.
