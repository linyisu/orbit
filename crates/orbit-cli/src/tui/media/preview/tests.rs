use std::time::Instant;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Terminal, backend::TestBackend};
use ratatui_image::picker::{Picker, ProtocolType};

use super::*;

fn mouse(kind: MouseEventKind, point: (u16, u16)) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: point.0,
        row: point.1,
        modifiers: KeyModifiers::NONE,
    })
}

fn draw_ready(preview: &mut Preview, terminal: &mut Terminal<TestBackend>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        super::super::begin_frame();
        terminal
            .draw(|frame| {
                preview.draw(frame);
                super::super::paint(frame.buffer_mut());
            })
            .unwrap();
        if let Some(picture) = preview.context.preview_picture(
            &preview.source,
            preview.body.width,
            preview.body.height,
            preview.view,
        ) {
            preview.picture = Some(picture);
            break;
        }
        assert!(
            preview
                .context
                .preview_error(
                    &preview.source,
                    preview.body.width,
                    preview.body.height,
                    preview.view
                )
                .is_none()
        );
        assert!(Instant::now() < deadline, "preview worker did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    super::super::begin_frame();
    terminal
        .draw(|frame| {
            preview.draw(frame);
            super::super::paint(frame.buffer_mut());
        })
        .unwrap();
}

#[test]
fn image_previews_zoom_pan_reset_and_clip_on_every_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("quadrants.png");
    image::RgbaImage::from_fn(400, 400, |x, y| {
        image::Rgba([
            if x < 200 { 255 } else { 0 },
            if y < 200 { 255 } else { 0 },
            0,
            255,
        ])
    })
    .save(&path)
    .unwrap();

    for protocol in [
        ProtocolType::Kitty,
        ProtocolType::Sixel,
        ProtocolType::Iterm2,
        ProtocolType::Halfblocks,
    ] {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(protocol);
        let mut preview = Preview::new(
            Context::new(picker),
            Source::Image(path.to_string_lossy().into_owned()),
        );
        let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
        draw_ready(&mut preview, &mut terminal);
        assert_eq!(preview.view, View::default());
        assert_eq!(preview.picture.as_ref().unwrap().pan_extent, (0.0, 0.0));
        let point = (
            preview.body.x + preview.body.width / 2,
            preview.body.y + preview.body.height / 2,
        );
        for _ in 0..4 {
            assert_eq!(
                preview.handle(&mouse(MouseEventKind::ScrollUp, point)),
                (true, false)
            );
        }
        assert_eq!(preview.view.zoom, 300);
        draw_ready(&mut preview, &mut terminal);
        let picture = preview.picture.as_ref().unwrap();
        assert!(picture.pan_extent.0 > 0.0 && picture.pan_extent.1 > 0.0);
        assert!(
            picture.size.width <= preview.body.width && picture.size.height <= preview.body.height
        );

        preview.handle(&Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        preview.handle(&Event::Key(KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        )));
        assert_eq!(preview.view.pan, (PAN_STEP, PAN_STEP));
        preview.handle(&mouse(MouseEventKind::Down(MouseButton::Left), point));
        preview.handle(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            (point.0 - 8, point.1 - 3),
        ));
        assert!(preview.view.pan.0 > PAN_STEP && preview.view.pan.1 > PAN_STEP);
        preview.handle(&mouse(MouseEventKind::Up(MouseButton::Left), point));
        assert!(preview.drag.is_none());
        draw_ready(&mut preview, &mut terminal);

        for _ in 0..20 {
            preview.handle(&Event::Key(KeyEvent::new(
                KeyCode::Char('+'),
                KeyModifiers::NONE,
            )));
        }
        assert_eq!(preview.view.zoom, *ZOOM_LEVELS.last().unwrap());
        for _ in 0..20 {
            preview.handle(&Event::Key(KeyEvent::new(
                KeyCode::Char('-'),
                KeyModifiers::NONE,
            )));
        }
        assert_eq!(preview.view.zoom, ZOOM_LEVELS[0]);
        preview.handle(&Event::Key(KeyEvent::new(
            KeyCode::Char('0'),
            KeyModifiers::NONE,
        )));
        assert_eq!(preview.view, View::default());

        for (control, expected) in [
            (Control::ZoomIn, 125),
            (Control::ZoomOut, 100),
            (Control::Fit, 100),
        ] {
            let button = preview
                .controls
                .iter()
                .find(|(_, c)| *c == control)
                .unwrap()
                .0;
            let point = (button.x, button.y);
            preview.handle(&mouse(MouseEventKind::Moved, point));
            terminal.draw(|frame| preview.draw(frame)).unwrap();
            assert_eq!(
                terminal.backend().buffer()[(button.x, button.y)].bg,
                preview.control_style(control).bg.unwrap()
            );
            assert_eq!(
                preview.handle(&mouse(MouseEventKind::Down(MouseButton::Left), point)),
                (true, false)
            );
            assert_eq!(preview.view.zoom, expected);
        }

        let mut small = Terminal::new(TestBackend::new(40, 12)).unwrap();
        preview.handle(&Event::Resize(40, 12));
        draw_ready(&mut preview, &mut small);
        super::super::LAYER.with(|layer| {
            let layer = layer.borrow();
            assert!(!layer.current.is_empty());
            assert!(
                layer
                    .current
                    .iter()
                    .all(|placement| placement.area.intersection(preview.body) == placement.area)
            );
        });
        assert_eq!(
            preview.handle(&mouse(MouseEventKind::Down(MouseButton::Left), (0, 0))),
            (true, true)
        );
        super::super::clear_layers();
    }
}
