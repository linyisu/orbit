use gpui::{
    Font, FontFallbacks, FontWeight, PlatformTextSystem, RenderGlyphParams, TextRun, TextSystem,
    WindowTextSystem, font, point, px,
};
use gpui_wgpu::CosmicTextSystem;
use std::{borrow::Cow, sync::Arc};

fn assert_shapes(text_system: &WindowTextSystem, source: &str, font: Font) -> gpui::ShapedLine {
    let shaped = text_system.shape_line(
        source.to_owned().into(),
        px(18.),
        &[TextRun {
            len: source.len(),
            font,
            ..Default::default()
        }],
        None,
    );
    assert_eq!(
        shaped.text.as_ref(),
        source,
        "fallback must preserve source bytes"
    );
    assert!(
        shaped.width() > px(0.),
        "{source:?} must have visible width"
    );
    assert!(!shaped.runs.is_empty(), "{source:?} must produce glyphs");
    assert!(
        shaped
            .runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .all(|glyph| glyph.id.0 != 0),
        "missing glyph in {source:?}: {:?}",
        shaped.runs,
    );
    shaped
}

#[test]
fn loading_a_non_latin_family_preserves_its_native_fallback_face() {
    let platform = Arc::new(CosmicTextSystem::new_without_system_fonts("IBM Plex Sans"));
    platform
        .add_fonts(vec![
            Cow::Borrowed(include_bytes!(
                "fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf"
            )),
            Cow::Borrowed(include_bytes!("fonts/noto/NotoSansSymbols2-Regular.ttf")),
        ])
        .unwrap();
    let symbol = font("Noto Sans Symbols 2");
    let symbol_id = platform
        .font_id(&symbol)
        .expect("a valid symbol font needs no Latin m");
    assert!(platform.glyph_for_char(symbol_id, 'm').is_none());
    assert!(
        platform
            .all_font_names()
            .iter()
            .any(|name| name == "Noto Sans Symbols 2")
    );
    let character = ['⭡', '⯀', '⏵', '🞄', '🜁', '☙', '⛶', '⟰']
        .into_iter()
        .find(|&character| platform.glyph_for_char(symbol_id, character).is_some())
        .expect("the bundled font supplies technical symbols");
    let text_system = WindowTextSystem::new(Arc::new(TextSystem::new(platform)));
    // The picker can show this family's name even though its primary font has
    // no Latin letters. Ordinary glyph fallback also remains available later.
    assert_shapes(&text_system, "Noto Sans Symbols 2", symbol);
    let mut latin = font("IBM Plex Sans");
    latin.fallbacks = Some(FontFallbacks::from_fonts(vec![
        "Noto Sans Symbols 2".into(),
    ]));
    assert_shapes(&text_system, &format!("symbol {character}"), latin);
}

#[test]
fn emoji_remains_available_after_family_preview_and_explicit_fallback_loading() {
    let platform = Arc::new(CosmicTextSystem::new("DejaVu Sans"));
    let families = platform.all_font_names();
    if !families.iter().any(|name| name == "Noto Color Emoji") {
        eprintln!("SKIP system emoji regression: Noto Color Emoji is not installed");
        return;
    }
    let text_system = WindowTextSystem::new(Arc::new(TextSystem::new(platform.clone())));
    let primary = families
        .iter()
        .find(|name| name.as_str() == "DejaVu Sans")
        .or_else(|| {
            families
                .iter()
                .find(|name| name.as_str() == "Noto Sans CJK SC")
        })
        .expect("a system text family is required alongside the emoji font");
    // Reproduce the destructive sequence: implicit fallback first, then an
    // emoji-family preview, then explicit fallback and later implicit shaping.
    assert_shapes(&text_system, "📦", font(primary.clone()));
    let emoji_id = platform
        .font_id(&font("Noto Color Emoji"))
        .expect("the emoji face must remain a loadable family");
    assert!(platform.glyph_for_char(emoji_id, 'm').is_none());
    assert_shapes(&text_system, "Noto Color Emoji", font("Noto Color Emoji"));
    assert!(
        platform
            .all_font_names()
            .iter()
            .any(|name| name == "Noto Color Emoji")
    );
    let fallbacks = FontFallbacks::from_fonts(
        [
            "Noto Color Emoji",
            "Noto Sans CJK SC",
            "Noto Sans Symbols 2",
            "DejaVu Sans",
        ]
        .into_iter()
        .filter(|name| families.iter().any(|family| family == name))
        .map(str::to_owned)
        .collect(),
    );
    for family in [
        "Noto Sans CJK SC",
        "Adwaita Sans",
        "Adwaita Mono",
        "DejaVu Sans Mono",
        "JetBrainsMono Nerd Font",
    ]
    .into_iter()
    .filter(|name| families.iter().any(|family| family == name))
    {
        for weight in [FontWeight::NORMAL, FontWeight::BOLD] {
            for explicit in [false, true] {
                let mut requested = font(family);
                requested.weight = weight;
                requested.fallbacks = explicit.then(|| fallbacks.clone());
                for source in ["📦", "🚀", "🧠", "✅", "👩‍💻", "🏳️‍🌈", "1️⃣", "→ ∑ ↔ ⚠"]
                {
                    let shaped = assert_shapes(&text_system, source, requested.clone());
                    if source == "📦" {
                        // A nonzero ID alone is insufficient: color glyphs must
                        // also survive the actual native rasterization path.
                        for run in &shaped.runs {
                            for glyph in &run.glyphs {
                                let params = RenderGlyphParams {
                                    font_id: run.font_id,
                                    glyph_id: glyph.id,
                                    font_size: px(18.),
                                    subpixel_variant: point(0, 0),
                                    scale_factor: 1.,
                                    is_emoji: glyph.is_emoji,
                                    subpixel_rendering: false,
                                    dilation: 0,
                                };
                                let bounds = platform.glyph_raster_bounds(&params).unwrap();
                                let (size, bitmap) =
                                    platform.rasterize_glyph(&params, bounds).unwrap();
                                assert!(
                                    size.width.0 > 0 && size.height.0 > 0 && !bitmap.is_empty()
                                );
                            }
                        }
                    }
                }
                if families.iter().any(|name| name == "Noto Sans CJK SC") {
                    assert_shapes(&text_system, "中文标题与代码注释", requested);
                }
            }
        }
    }
}
