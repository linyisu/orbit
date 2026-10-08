use super::*;
use ratatui::{Terminal, backend::TestBackend};

#[test]
fn pointer_handoff_does_not_reuse_stationary_motion_or_keyboard_highlight() {
    let mut pointer = Pointer::default();
    let point = (4, 4).into();
    let first = [(Rect::new(3, 3, 8, 2), 1)];
    assert!(pointer.moved(point, &first));
    assert_eq!(pointer.highlight(Some(2)), Some(1));
    pointer.press(point);
    pointer.invalidate();
    let next = [(first[0].0, 2)];
    assert!(!pointer.moved(point, &next));
    assert_eq!(pointer.highlight(Some(2)), None);
    pointer.moved((0, 0).into(), &next);
    assert_eq!(pointer.highlight(Some(2)), None);
    pointer.keyboard();
    assert_eq!(pointer.highlight(Some(2)), Some(2));
}

#[test]
fn disabled_buttons_have_no_hit_and_action_bars_stay_inside_their_allocation() {
    let mut terminal = Terminal::new(TestBackend::new(24, 10)).unwrap();
    terminal
        .draw(|frame| {
            let allocation = Rect::new(3, 4, 14, 2);
            let mut bar = ActionBar::new(allocation);
            assert!(
                bar.button(
                    frame,
                    "不可用",
                    Control {
                        disabled: true,
                        highlighted: true,
                        ..Control::default()
                    }
                )
                .is_none()
            );
            let hit = bar
                .button(
                    frame,
                    "下一题",
                    Control {
                        highlighted: true,
                        ..Control::default()
                    },
                )
                .unwrap();
            assert_eq!(hit.intersection(allocation), hit);
            assert_eq!(frame.buffer_mut()[(hit.x, hit.y)].bg, THEME.accent);
            assert!(bar.button(frame, "第三项", Control::default()).is_none());
        })
        .unwrap();
}

#[test]
fn wrapped_choices_keep_padding_and_highlight_on_every_line() {
    let lines = choice_lines("中文 English long description", highlighted(), 16);
    assert!(lines.len() > 1);
    for line in lines {
        assert!(line.to_string().starts_with("  "));
        assert!(line.to_string().ends_with("  "));
        assert!(line.width() <= 16);
        assert_eq!(line.style.bg, Some(THEME.accent));
    }
}

#[test]
fn modal_insets_are_bounded_on_tiny_and_offset_surfaces() {
    for (width, height) in [(1, 1), (4, 2), (20, 8), (100, 40)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                let outer = centered(frame.area(), 92, 25);
                let inner = modal(frame, outer);
                assert_eq!(inner.intersection(frame.area()), inner);
            })
            .unwrap();
    }
    let bounds = Rect::new(8, 5, 100, 40);
    let outer = centered(bounds, 92, 25);
    assert_eq!(outer.intersection(bounds), outer);
}
