use super::{DiffFile, DiffMode, DiffSide, LayoutKey, code_width, displayed_gutters};
use gpui::px;

#[test]
fn single_gutter_tracks_source_numbers_after_insertion_and_deletion() {
    let patch =
        "--- a/a.rs\n+++ b/a.rs\n@@ -28,4 +28,4 @@\n keep\n+inserted\n next\n-removed\n tail\n";
    let files = DiffFile::parse(patch).unwrap();
    let file = &files[0];
    for (old, new, expected_side, expected_number) in [
        (Some(0), Some(0), DiffSide::Modified, 28),
        (None, Some(1), DiffSide::Modified, 29),
        (Some(1), Some(2), DiffSide::Modified, 30),
        (Some(2), None, DiffSide::Original, 30),
        (Some(3), Some(3), DiffSide::Modified, 31),
    ] {
        let source = [
            Some((DiffSide::Original, old)),
            Some((DiffSide::Modified, new)),
        ];
        let displayed = displayed_gutters(source, true);
        let (side, index) = displayed[0].unwrap();
        assert!(displayed[1].is_none());
        assert_eq!(side, expected_side);
        assert_eq!(file.line_number(side, index.unwrap()), expected_number);
        // The default dual-lane mode still exposes both source mappings.
        assert_eq!(displayed_gutters(source, false), source);
    }
}

#[test]
fn single_gutter_width_and_layout_invalidation() {
    let width = |mode, single| code_width(px(100.), px(30.), px(16.), mode, single);
    assert_eq!(width(DiffMode::Unified, true), px(162.));
    assert_eq!(width(DiffMode::Unified, false), px(192.));
    assert_eq!(width(DiffMode::Split, true), width(DiffMode::Split, false));
    let layout = |single| {
        LayoutKey::new(
            px(16.),
            px(14.),
            "mono".into(),
            DiffMode::Unified,
            true,
            single,
            false,
        )
    };
    assert_ne!(layout(true), layout(false));
}
