# Orbit text fallback patch

Source: crates.io `gpui-pre-wgpu` 0.3.8, the gpui-pre snapshot of
Zed commit `279fe070bb389b79652e52065b2f001edcc0b11b`.
Upstream: https://github.com/zed-industries/zed/tree/279fe070bb389b79652e52065b2f001edcc0b11b/crates/gpui_wgpu
The original Apache-2.0 license is in `LICENSE-APACHE`.

`CosmicTextSystemState::load_family` no longer removes a font face from the
shared database merely because it has no ASCII `m`. Emoji and symbol fonts
often have no Latin letters. Resolving `Noto Color Emoji` as an explicit
fallback or showing its font-picker preview previously removed it, breaking
later implicit fallback throughout the application.

Fallback remains native and grapheme-aware. The application supplies its
installed-font chain through `Font.fallbacks`; the source text is unchanged.

The upstream unit-test fonts are copied from the local Zed checkout, with
their original license files, into `tests/fonts`. Their include paths were
adjusted to make this standalone vendored crate's tests self-contained. A Noto
Sans Symbols 2 fixture and its SIL Open Font License provide a portable font
that has symbol glyphs but no Latin `m`.
`tests/font_fallback.rs` exercises the actual GPUI text system without a window
or renderer. System-font cases explicitly skip when their named test font is
not installed; the bundled Latin fixtures remain portable.
