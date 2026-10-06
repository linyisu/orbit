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
    // Allow scheduling headroom on busy CI runners.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        super::super::begin_frame();
        terminal
            .draw(|frame| {
                preview.draw(frame);
                super::super::paint(frame.buffer_mut());
            })
            .unwrap();
        if preview
            .picture
            .as_ref()
            .is_some_and(|picture| picture.native.is_some())
        {
            break;
        }
        if preview.requested_quality.is_none() {
            wait_for_worker(preview, deadline);
            continue;
        }
        let view = View {
            quality: preview.requested_quality.unwrap(),
            ..preview.view
        };
        let mut key = preview.context.key(
            &preview.source,
            preview.body.width,
            preview.body.height,
            true,
        );
        key.view = Some(view);
        if let Some(picture) = preview.context.lookup(key, false, None) {
            preview.picture = Some(picture);
            break;
        }
        wait_for_worker(preview, deadline);
    }
    super::super::begin_frame();
    terminal
        .draw(|frame| {
            preview.draw(frame);
            super::super::paint(frame.buffer_mut());
        })
        .unwrap();
}

fn wait_for_worker(preview: &Preview, deadline: Instant) {
    let view = View {
        quality: preview.requested_quality.unwrap_or(preview.view.quality),
        ..preview.view
    };
    let error = preview
        .context
        .preview_error(
            &preview.source,
            preview.body.width,
            preview.body.height,
            view,
        )
        .or_else(|| {
            preview.context.error(
                &preview.source,
                preview.body.width,
                preview.body.height,
                true,
            )
        });
    let describe = || {
        format!(
            "protocol={:?}, source={:?}, body={:?}, view={:?}, requested_quality={:?}, error={error:?}",
            preview
                .context
                .shared
                .as_ref()
                .map(|shared| shared.picker.protocol_type()),
            preview.source,
            preview.body,
            preview.view,
            preview.requested_quality,
        )
    };
    assert!(error.is_none(), "preview worker failed: {}", describe());
    assert!(
        Instant::now() < deadline,
        "preview worker did not finish within 60 seconds: {}",
        describe()
    );
    std::thread::sleep(Duration::from_millis(5));
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
        assert_eq!(preview.pan_extent(), (0.0, 0.0));
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
        assert_eq!(preview.view.zoom, 126);
        while preview.view.zoom < 300 {
            preview.handle(&mouse(MouseEventKind::ScrollUp, point));
        }
        draw_ready(&mut preview, &mut terminal);
        let picture = preview.picture.as_ref().unwrap();
        let extent = preview.pan_extent();
        assert!(extent.0 > 0.0 && extent.1 > 0.0);
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
        preview.handle(&mouse(
            MouseEventKind::Up(MouseButton::Left),
            (point.0 - 8, point.1 - 3),
        ));
        assert!(preview.drag.is_none());
        draw_ready(&mut preview, &mut terminal);

        for _ in 0..100 {
            preview.handle(&Event::Key(KeyEvent::new(
                KeyCode::Char('+'),
                KeyModifiers::NONE,
            )));
        }
        assert_eq!(preview.view.zoom, MAX_ZOOM);
        for _ in 0..100 {
            preview.handle(&Event::Key(KeyEvent::new(
                KeyCode::Char('-'),
                KeyModifiers::NONE,
            )));
        }
        assert_eq!(preview.view.zoom, MIN_ZOOM);
        preview.handle(&Event::Key(KeyEvent::new(
            KeyCode::Char('0'),
            KeyModifiers::NONE,
        )));
        assert_eq!(preview.view, View::default());

        for (control, expected) in [
            (Control::ZoomIn, 106),
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
            assert!(!layer.current.is_empty() || layer.current_preview.is_some());
            if let Some(native) = &layer.current_preview {
                assert_eq!(native.area().intersection(preview.body), native.area());
            }
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

#[test]
fn large_pixel_cells_use_the_entire_preview_width_before_clipping() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("wide.png");
    let svg = dir.path().join("wide.svg");
    image::RgbaImage::from_pixel(1600, 450, image::Rgba([230, 100, 50, 255]))
        .save(&png)
        .unwrap();
    std::fs::write(&svg, r#"<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="450"><rect width="1600" height="450" fill="red"/></svg>"#).unwrap();

    for path in [png, svg] {
        // TestBackend cannot query the terminal's reported pixel cell size.
        #[allow(deprecated)]
        let mut picker = Picker::from_fontsize((32, 64).into());
        picker.set_protocol_type(ProtocolType::Kitty);
        let mut preview = Preview::new(
            Context::new(picker),
            Source::Image(path.to_string_lossy().into_owned()),
        );
        let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
        for zoom in [100, 125] {
            preview.view.zoom = zoom;
            draw_ready(&mut preview, &mut terminal);
            let picture = preview.picture.as_ref().unwrap();
            assert!(u32::from(preview.body.width) * 32 > 2048);
            let extent = preview.pan_extent();
            let native_area = picture.native.as_ref().map(|image| {
                super::super::kitty_preview::Placement {
                    image: image.clone(),
                    body: preview.body,
                    view: preview.view,
                }
                .area()
            });
            assert_eq!(
                native_area.map_or(picture.size.width, |area| area.width),
                preview.body.width
            );
            assert_eq!(extent.1, 0.0);
            if zoom == 100 {
                assert_eq!(extent.0, 0.0);
            } else {
                assert!((extent.0 - f32::from(preview.body.width) * 0.25).abs() < 0.01);
            }
            assert!(picture.selected_protocol.is_none());
            let bytes = super::super::Value::Ready(picture.clone())
                .bytes(&preview.context.shared.as_ref().unwrap().picker);
            assert!(bytes <= super::super::MAX_CACHE_BYTES);
            super::super::LAYER.with(|layer| {
                assert!(layer.borrow().current.iter().all(|placement| {
                    placement.area.x == preview.body.x && placement.area.width == preview.body.width
                }));
            });
        }
        super::super::clear_layers();
    }
}

#[test]
fn interaction_keeps_the_last_frame_and_idle_requests_full_quality_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("picture.png");
    image::RgbaImage::from_pixel(200, 200, image::Rgba([100, 150, 200, 255]))
        .save(&path)
        .unwrap();
    let mut preview = Preview::new(
        Context::new(Picker::halfblocks()),
        Source::Image(path.to_string_lossy().into()),
    );
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    draw_ready(&mut preview, &mut terminal);
    assert_eq!(preview.requested_quality, Some(Quality::Full));
    assert!(!preview.tick());

    preview.zoom(true);
    draw_ready(&mut preview, &mut terminal);
    assert_eq!(preview.requested_quality, Some(Quality::Interactive));
    assert!(!preview.tick());
    let previous = preview.picture.as_ref().unwrap().clone();
    preview.zoom(true);
    terminal.draw(|frame| preview.draw(frame)).unwrap();
    assert!(Arc::ptr_eq(&previous, preview.picture.as_ref().unwrap()));

    preview.changed_at = Some(Instant::now() - QUALITY_SETTLE_TIME);
    assert!(preview.tick());
    draw_ready(&mut preview, &mut terminal);
    assert_eq!(preview.requested_quality, Some(Quality::Full));
    assert!(!preview.tick());
    assert!(!Arc::ptr_eq(&previous, preview.picture.as_ref().unwrap()));

    let point = (preview.body.x, preview.body.y);
    preview.handle(&mouse(MouseEventKind::Down(MouseButton::Left), point));
    terminal.draw(|frame| preview.draw(frame)).unwrap();
    assert_eq!(preview.requested_quality, Some(Quality::Interactive));
    assert!(!preview.tick());
    preview.handle(&mouse(MouseEventKind::Up(MouseButton::Left), point));
    preview.changed_at = Some(Instant::now() - QUALITY_SETTLE_TIME);
    assert!(preview.tick());
    draw_ready(&mut preview, &mut terminal);
    assert!(!preview.tick());
    super::super::clear_layers();
}

#[test]
fn kitty_drag_tracks_the_pointer_without_requesting_or_uploading_another_image() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("picture.png");
    image::RgbaImage::from_pixel(800, 400, image::Rgba([100, 150, 200, 255]))
        .save(&path)
        .unwrap();
    let mut picker = Picker::halfblocks();
    picker.set_protocol_type(ProtocolType::Kitty);
    let mut preview = Preview::new(
        Context::new(picker),
        Source::Image(path.to_string_lossy().into()),
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    draw_ready(&mut preview, &mut terminal);
    preview.view.zoom = 400;
    draw_ready(&mut preview, &mut terminal);
    let picture = preview.picture.as_ref().unwrap().clone();
    let shared = preview.context.shared.as_ref().unwrap().clone();
    let revision = shared.revision.load(std::sync::atomic::Ordering::Acquire);
    shared.cache.lock().unwrap().clear();
    let origin = (
        preview.body.x + preview.body.width / 2,
        preview.body.y + preview.body.height / 2,
    );
    preview.handle(&mouse(MouseEventKind::Down(MouseButton::Left), origin));
    for offset in 1..20 {
        preview.handle(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            (origin.0 - offset, origin.1),
        ));
        super::super::begin_frame();
        terminal
            .draw(|frame| {
                preview.draw(frame);
                super::super::paint(frame.buffer_mut());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(!buffer[(0, 0)].symbol().contains("a=t"));
        assert!(buffer[(0, 0)].symbol().contains("a=p"));
        assert!(Arc::ptr_eq(&picture, preview.picture.as_ref().unwrap()));
        super::super::LAYER.with(|layer| {
            assert_eq!(
                layer.borrow().current_preview.as_ref().unwrap().view,
                preview.view
            )
        });
    }
    assert!(preview.view.pan.0 > 0);
    assert!(shared.cache.lock().unwrap().is_empty());
    assert_eq!(
        shared.revision.load(std::sync::atomic::Ordering::Acquire),
        revision
    );
    let before_release = preview.view.pan;
    preview.handle(&mouse(
        MouseEventKind::Up(MouseButton::Left),
        (origin.0 - 20, origin.1),
    ));
    assert!(preview.drag.is_none());
    assert!(preview.view.pan.0 > before_release.0);
    assert_eq!(preview.view.pan.1, before_release.1);
    super::super::clear_layers();
}
