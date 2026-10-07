use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend, buffer::CellWidth, style::Style, text::Span};
use resvg::usvg;
use unicode_width::UnicodeWidthStr;

use super::*;
use crate::tui::{
    app::{TranscriptItem, UiState},
    markdown::{self, LinkedLine},
    selection::Selection,
    transcript::TranscriptLayout,
};

fn context(protocol: ProtocolType) -> Context {
    let mut picker = Picker::halfblocks();
    picker.set_protocol_type(protocol);
    Context::new(picker)
}

#[test]
fn terminal_detection_probes_graphics_without_exercising_zlib_inflation() {
    for tmux in [false, true] {
        let query =
            ratatui_image::picker::cap_parser::Parser::query(tmux, terminal_query_options());
        assert!(query.contains("i=31,s=1,v=1,a=q,t=d,f=24;AAAA"));
        assert!(query.contains("]11;?"));
        assert!(!query.contains("i=32"));
        assert!(!query.contains("o=z"));
    }
}

fn ready(context: &Context, source: Source, width: u16, height: u16) -> Arc<Prepared> {
    let key = context.key(&source, width, height, false);
    let shared = context.shared.as_ref().unwrap();
    let picture = Arc::new(
        decode::Decoder::default()
            .prepare(&key, &shared.picker)
            .unwrap(),
    );

    shared.cache.lock().unwrap().insert(
        key,
        Cached {
            value: Value::Ready(picture.clone()),
            touched: 0,
        },
    );
    picture
}

fn wait_for(context: &Context, source: &Source, width: u16, height: u16) -> Arc<Prepared> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(picture) = context.picture(source, width, height, false) {
            return picture;
        }
        assert!(context.error(source, width, height, false).is_none());
        assert!(Instant::now() < until, "media worker did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn remote_previews_use_session_proxy_and_isolate_cache_after_rebind() {
    use std::io::{Read, Write};

    let proxies: Vec<_> = [[255, 0, 0, 255], [0, 255, 0, 255]]
        .into_iter()
        .map(|color| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let client = Arc::new(orbit_tools::web_fetch::WebClient::new(
                &orbit_tools::web_fetch::WebConfig {
                    proxy: Some(format!("http://{}", listener.local_addr().unwrap())),
                    ..Default::default()
                },
            ).unwrap());
            let worker = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "preview did not use its proxy");
                            std::thread::sleep(Duration::from_millis(5));
                        }

                        Err(error) => panic!("{error}"),
                    }
                };

                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut request = [0; 4096];
                let count = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..count]).starts_with(
                    "GET http://proxy-only.invalid/picture.png HTTP/1.1"
                ));
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                    8, 8, image::Rgba(color),
                )).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
                stream.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.get_ref().len()
                ).as_bytes()).unwrap();
                stream.write_all(bytes.get_ref()).unwrap();
            });
            (client, worker)
        })
        .collect();

    let mut first = context(ProtocolType::Halfblocks);
    first.set_client(proxies[0].0.clone());
    let source = Source::Image("http://proxy-only.invalid/picture.png".into());
    let first_picture = wait_for(&first, &source, 10, 4);
    let mut second = first.clone();
    second.set_client(proxies[1].0.clone());
    assert!(Arc::ptr_eq(
        first.shared.as_ref().unwrap(),
        second.shared.as_ref().unwrap()
    ));
    assert!(!first.same_cache(&second));
    assert!(second.cached_picture(&source, 10, 4).is_none());
    let second_picture = wait_for(&second, &source, 10, 4);
    assert!(!Arc::ptr_eq(&first_picture, &second_picture));
    let math = Source::Math {
        tex: "x^2".into(),
        display: false,
    };
    assert_eq!(
        first.key(&math, 10, 4, false),
        second.key(&math, 10, 4, false)
    );
    for (_, worker) in proxies {
        worker.join().unwrap();
    }
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn draw_ui(state: &mut UiState, terminal: &mut Terminal<TestBackend>) {
    begin_frame();
    terminal
        .draw(|f| {
            crate::tui::render::draw(f, state);
            state.selection.paint(f);
            paint(f.buffer_mut());
        })
        .unwrap();
}

fn assert_ui_graphics(state: &mut UiState) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        draw_ui(state, &mut terminal);
        if terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|c| c.symbol().contains('\u{10eeee}'))
        {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "new/resumed UI failed to render media: {:?}",
            state.media
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    clear_layers();
    terminal.backend().buffer().clone()
}

#[tokio::test]
async fn new_tabs_share_terminal_media_and_keep_graphics_when_switching_back() {
    use crate::{
        session::SessionState,
        tui::{CommandContext, tabs::Tabs},
    };
    use orbit_model::client::ModelClient;
    let dir = tempfile::tempdir().unwrap();
    let mut session = SessionState::open(dir.path().join("sessions.db"), None, "mock".into())
        .await
        .unwrap();
    let mut state = UiState::new("mock");
    state.media = context(ProtocolType::Kitty);
    let media = state.media.clone();
    state
        .transcript
        .push(TranscriptItem::Assistant("公式 $E=mc^2$".into()));
    assert_ui_graphics(&mut state);
    let first = session.id().to_owned();
    let mut tabs = Tabs::new(&session);
    let mut history = Vec::new();
    let mut model = ModelClient::new(Default::default()).unwrap();
    let tools = orbit_agent::tool::ToolRegistry::new();
    let instructions = crate::instructions::Instructions::default();
    let skills = crate::skills::Skills::default();
    crate::tui::new_tab(
        &mut tabs,
        &mut session,
        &mut state,
        &mut model,
        &mut history,
        dir.path(),
        CommandContext {
            tools: &tools,
            instructions: &instructions,
            skills: &skills,
            mcp_statuses: &[],
            model_options: &[],
        },
    )
    .await
    .unwrap();
    assert!(state.media.same_cache(&media));
    assert!(state.media_preview.is_none());
    let second = session.id().to_owned();
    state
        .transcript
        .push(TranscriptItem::Assistant("$$\n\\frac{a}{b}\n$$".into()));
    assert_ui_graphics(&mut state);
    assert!(tabs.restore(&first, &mut session, &mut state, &mut model, &mut history));
    assert!(state.media.same_cache(&media));
    assert_ui_graphics(&mut state);
    assert!(tabs.restore(&second, &mut session, &mut state, &mut model, &mut history));
    assert_ui_graphics(&mut state);
}

#[tokio::test]
async fn saved_sessions_restore_media_capabilities_before_replaying_formula_output() {
    use crate::session::SessionState;
    use orbit_model::{
        client::ModelClient,
        types::{ChatMessage, StreamEvent},
    };
    let dir = tempfile::tempdir().unwrap();
    let mut session = SessionState::open(dir.path().join("sessions.db"), None, "mock".into())
        .await
        .unwrap();
    let id = session.id().to_owned();
    let formula = "## 数学公式\n$$\n\\int_{-\\infty}^{\\infty}e^{-x^2}\\,dx=\\sqrt{\\pi}\n$$";
    session
        .begin("formula".into(), "mock".into())
        .await
        .unwrap();
    let journal = session.execution_journal().unwrap();
    journal.record(orbit_agent::events::AgentEvent::Model(
        StreamEvent::TextDelta(formula.into()),
    ));
    journal.flush().await.unwrap();
    session
        .finish(
            "completed",
            Some(vec![
                ChatMessage::user("formula"),
                ChatMessage::assistant(formula),
            ]),
        )
        .await
        .unwrap();
    crate::tui::session_actions::new_test_session(&mut session, "mock".into())
        .await
        .unwrap();
    let mut state = UiState::new("mock");
    state.media = context(ProtocolType::Kitty);
    let media = state.media.clone();
    let mut model = ModelClient::new(Default::default()).unwrap();
    let mut history = Vec::new();
    let scope = crate::project_state::MemoryScope::new(session.id().into());
    crate::tui::resume_session(
        &id,
        &mut state,
        &mut model,
        &mut history,
        &mut session,
        &Default::default(),
        &Default::default(),
        &[],
        &scope,
        &orbit_agent::tool::ToolRegistry::new(),
    )
    .await
    .unwrap();
    assert!(state.media.same_cache(&media));
    assert!(
        state
            .transcript
            .iter()
            .any(|item| matches!(item, TranscriptItem::Assistant(text) if text.contains("\\int")))
    );
    assert_ui_graphics(&mut state);
}

#[tokio::test]
async fn restored_read_image_results_upload_complete_rgba_with_selection_and_row_clipping() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use orbit_agent::events::AgentEvent;
    use orbit_model::{client::ModelClient, types::ToolCall};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    image::RgbImage::from_pixel(1800, 520, image::Rgb([80, 120, 160]))
        .save(&path)
        .unwrap();
    let mut session = crate::session::SessionState::open_in(
        dir.path().join("sessions.db"),
        None,
        "mock".into(),
        dir.path(),
    )
    .await
    .unwrap();
    let id = session.id().to_owned();
    session.begin("image".into(), "mock".into()).await.unwrap();
    let call = ToolCall::function(
        "image",
        "read_image",
        serde_json::json!({"path": "image.png"}),
    );
    let journal = session.execution_journal().unwrap();
    let checkpoint = orbit_agent::checkpoint::ExecutionCheckpoint {
        history: Vec::new(),
        turns: 1,
        tool_calls: 1,
        in_flight: None,
        attachments: Vec::new(),
    };
    for event in [
        AgentEvent::ToolStarted { call: call.clone() },
        AgentEvent::ToolFinished {
            call,
            content: serde_json::json!({"path": "image.png", "loaded": true}).to_string(),
            changes: Vec::new(),
        },
    ] {
        journal.save(checkpoint.clone(), Some(event)).await.unwrap();
    }
    session.finish("completed", Some(Vec::new())).await.unwrap();
    crate::tui::session_actions::new_test_session(&mut session, "mock".into())
        .await
        .unwrap();

    let mut state = UiState::new("mock");
    state.media = context(ProtocolType::Kitty);
    let mut model = ModelClient::new(Default::default()).unwrap();
    let mut history = Vec::new();
    let scope = crate::project_state::MemoryScope::new(session.id().into());
    crate::tui::resume_session(
        &id,
        &mut state,
        &mut model,
        &mut history,
        &mut session,
        &Default::default(),
        &Default::default(),
        &[],
        &scope,
        &orbit_agent::tool::ToolRegistry::new(),
    )
    .await
    .unwrap();
    let index = state
        .transcript
        .iter()
        .position(|item| matches!(item, TranscriptItem::Tool(row) if row.name == "read_image"))
        .unwrap();
    state.expanded.insert(index);
    let rendered = assert_ui_graphics(&mut state);
    let picture = state
        .media
        .shared
        .as_ref()
        .unwrap()
        .cache
        .lock()
        .unwrap()
        .values()
        .find_map(|entry| match &entry.value {
            Value::Ready(picture) => Some(picture.clone()),
            _ => None,
        })
        .unwrap();

    let cell = state.media.shared.as_ref().unwrap().picker.font_size();
    let expected = usize::from(picture.size.width)
        * usize::from(cell.width)
        * usize::from(picture.size.height)
        * usize::from(cell.height)
        * 4;
    let mut uploads = Vec::new();
    for (variant, protocol) in [
        &picture.protocol,
        picture.selected_protocol.as_ref().unwrap(),
    ]
    .into_iter()
    .enumerate()
    {
        let area = Rect::new(0, 0, picture.size.width, 1);
        let buffer = if variant == 0 {
            rendered.clone()
        } else {
            let mut buffer = Buffer::empty(area);
            SlicedImage::new(protocol, (0, -1).into()).render(area, &mut buffer);
            buffer
        };
        let output = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!output.contains("o=z"));
        assert!(output.contains("a=T,U=1,f=32"));
        assert!(output.contains('\u{10eeee}'));
        let mut pixels = Vec::new();
        let commands = output.split("\x1b_G").skip(1).collect::<Vec<_>>();
        assert!(commands.len() > 1, "large images must span multiple chunks");
        for (index, command) in commands.iter().enumerate() {
            let (control, payload) = command.split_once(';').unwrap();
            let payload = payload.split_once("\x1b\\").unwrap().0;
            let more = if index + 1 < commands.len() {
                "m=1"
            } else {
                "m=0"
            };
            assert!(control.ends_with(more));
            pixels.extend(STANDARD.decode(payload).unwrap());
        }
        assert_eq!(pixels.len(), expected);
        uploads.push(pixels);

        let mut second = Buffer::empty(area);
        SlicedImage::new(protocol, (0, -1).into()).render(area, &mut second);
        assert!(
            second
                .content
                .iter()
                .all(|cell| !cell.symbol().contains("a=T"))
        );
    }
    assert!(uploads[0].as_chunks::<4>().0.contains(&[80, 120, 160, 255]));
    assert_ne!(uploads[0], uploads[1]);
    clear_layers();
}

#[test]
fn multiline_math_fallback_keeps_line_breaks_click_targets_and_copyable_tex() {
    let source = "$$\n\\frac{a}{b}\n\\sqrt{x}\n$$";
    for thinking in [false, true] {
        for protocol in [ProtocolType::Halfblocks, ProtocolType::Kitty] {
            let media = context(protocol);
            let item = if thinking {
                TranscriptItem::Thinking(source.to_owned().into())
            } else {
                TranscriptItem::Assistant(source.into())
            };
            let mut layout = TranscriptLayout::default();
            layout.set_media(media);
            layout.update(&[item], None, 80);
            let area = Rect::new(0, 0, 80, layout.height() as u16);
            let mut buffer = Buffer::empty(area);
            layout.draw_buffer(&mut buffer, area, 0);
            let rows = buffer
                .content
                .chunks(80)
                .map(|cells| cells.iter().map(|c| c.symbol()).collect::<String>())
                .collect::<Vec<_>>();

            assert!(rows.iter().any(|row| row.contains(r"\frac{a}{b}")));
            assert!(rows.iter().any(|row| row.contains(r"\sqrt{x}")));
            assert!(
                rows.iter().all(|row| !row.contains(r"\n")),
                "thinking={thinking}, protocol={protocol:?}: {rows:?}"
            );
            assert!(
                (0..area.height)
                    .any(|y| (0..80).any(|x| layout.media_at(area, 0, (x, y)).is_some()))
            );
            let mut selection = Selection::default();
            selection.viewport = area;
            selection
                .handle(&mouse(MouseEventKind::Down(MouseButton::Left), 0, 0))
                .unwrap();
            selection
                .handle(&mouse(
                    MouseEventKind::Drag(MouseButton::Left),
                    79,
                    area.height - 1,
                ))
                .unwrap();
            selection.transcript_view(0, &layout);
            assert_eq!(selection.text().trim(), source);
        }
    }
}

#[test]
fn framed_tool_and_finished_thinking_keep_formula_targets_and_fallback_lines() {
    use crate::tui::app::{ThinkingRow, ToolRow, ToolStatus};
    let source = "$$\n\\frac{a}{b}\n\\sqrt{x}\n$$\n\n行内 $\\frac{1}{2} + \\frac{3}{4} + \\frac{5}{6} + \\frac{7}{8}$";
    let mut thought = ThinkingRow::from(source.to_owned());
    thought.finished = true;
    for item in [
        TranscriptItem::Thinking(thought),
        TranscriptItem::Tool(ToolRow {
            id: "math".into(),
            name: "web_fetch".into(),
            arguments: r#"{"url":"https://orbit.example/math"}"#.into(),
            status: ToolStatus::Finished,
            output: serde_json::json!({"url":"https://orbit.example/math", "content":source})
                .to_string(),
        }),
    ] {
        let mut state = UiState::new("mock");
        state.media = context(ProtocolType::Kitty);
        state.transcript.push(item.clone());
        state.expanded.insert(0);
        assert_ui_graphics(&mut state);
        let mut layout = TranscriptLayout::default();
        layout.set_media(context(ProtocolType::Halfblocks));
        layout.update(&[item], Some(0), 36);
        let area = Rect::new(0, 0, 36, layout.height() as u16);
        let mut buffer = Buffer::empty(area);
        layout.draw_buffer(&mut buffer, area, 0);
        let rows = buffer
            .content
            .chunks(36)
            .map(|cells| cells.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>();

        assert!(
            rows.iter().any(|row| row.contains(r"\frac{a}{b}")),
            "{rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains(r"\sqrt{x}")), "{rows:?}");
        assert!(rows.iter().all(|row| !row.contains(r"\n")), "{rows:?}");
        for tex in ["\\frac{a}{b}", "\\frac{1}{2}"] {
            assert!((0..area.height).any(|y| (0..area.width).any(|x| matches!(layout.media_at(area, 0, (x, y)), Some(Source::Math { tex: found, .. }) if found.contains(tex)))));
        }
    }
}

#[test]
fn screenshot_formulas_render_through_the_full_async_ui_pipeline() {
    let source = "## 内联公式\n质能方程 $E = mc^2$；欧拉恒等式 $e^{i\\pi} + 1 = 0$\n求根公式 $x = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}$\n希腊字母 $\\alpha\\ \\beta\\ \\gamma\\ \\theta\\ \\lambda\\ \\sigma\\ \\omega$\n常用符号 $\\in\\ \\notin\\ \\subset\\ \\supset\\ \\cup\\ \\cap\\ \\forall\\ \\exists\\ \\leq\\ \\geq\\ \\approx\\ \\sim\\ \\to\\ \\implies\\ \\iff$\n\n## 块级公式\n\n$$\n\\int_{-\\infty}^{\\infty}e^{-x^2}\\,dx=\\sqrt{\\pi}\n$$\n\n黎曼 zeta 函数 ($\\operatorname{Re}(s)>1$)：\n\n$$\n\\zeta(s)=\\sum_{n=1}^{\\infty}\\frac{1}{n^{s}}=\\prod_{p\\ \\text{prime}}\\frac{1}{1-p^{-s}}\n$$\n\n$$\nA=\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}\\qquad A^{-1}=\\frac{1}{-2}\\begin{pmatrix}4&-2\\\\-3&1\\end{pmatrix}\n$$\n\n$$\n\\begin{aligned}f'(x)&=\\lim_{h\\to0}\\frac{f(x+h)-f(x)}{h}\\\\[6pt]f(x)&=\\sum_{n=0}^{\\infty}\\frac{f^{(n)}(0)}{n!}x^n\\end{aligned}\n$$\n\n$$\n\\operatorname{sgn}(x)=\\begin{cases}-1,&x<0\\\\0,&x=0\\\\1,&x>0\\end{cases}\n$$\n\n$$\n\\begin{aligned}\\nabla\\cdot\\mathbf{E}&=\\frac{\\rho}{\\varepsilon_0}\\\\\\nabla\\cdot\\mathbf{B}&=0\\\\\\nabla\\times\\mathbf{E}&=-\\frac{\\partial\\mathbf{B}}{\\partial t}\\\\\\nabla\\times\\mathbf{B}&=\\mu_0\\mathbf{J}+\\mu_0\\varepsilon_0\\frac{\\partial\\mathbf{E}}{\\partial t}\\end{aligned}\n$$";
    let mut state = UiState::new("mock");
    state.media = context(ProtocolType::Kitty);
    state
        .transcript
        .push(TranscriptItem::Assistant(source.into()));
    let mut terminal = Terminal::new(TestBackend::new(180, 140)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        draw_ui(&mut state, &mut terminal);
        let cache = state.media.shared.as_ref().unwrap().cache.lock().unwrap();
        for (key, entry) in cache.iter() {
            if let Value::Failed(error) = &entry.value {
                panic!("{:?}: {error}", key.source);
            }
        }

        if cache.len() == 12 && cache.values().all(|v| matches!(v.value, Value::Ready(_))) {
            break;
        }
        assert!(Instant::now() < deadline, "cached {} formulas", cache.len());
        drop(cache);
        std::thread::sleep(Duration::from_millis(5));
    }

    draw_ui(&mut state, &mut terminal);
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|c| c.symbol().contains('\u{10eeee}'))
    );
    assert!(
        !terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>()
            .contains(r"\frac")
    );
    clear_layers();
}

#[test]
fn ratex_unicode_fonts_do_not_write_to_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_system_fonts();
    let override_font = fonts.faces().find_map(|face| match &face.source {
        usvg::fontdb::Source::File(path) | usvg::fontdb::Source::SharedFile(path, _) => {
            Some(format!("{}#{}", path.display(), face.index))
        }

        usvg::fontdb::Source::Binary(_) => None,
    });
    let mut sources = vec![
        None,
        Some(dir.path().join("missing.ttf").display().to_string()),
    ];
    if let Some(font) = override_font {
        sources.push(Some(font));
    }

    for source in sources {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "tui::media::tests::ratex_unicode_formula_uses_glyphs_or_text_fallback",
            "--nocapture",
        ]);
        if let Some(font) = &source {
            command.env("RATEX_UNICODE_FONT", font);
        } else {
            command.env_remove("RATEX_UNICODE_FONT");
        }
        let output = command.output().unwrap();

        assert!(output.status.success(), "{source:?}: {output:?}");
        assert!(
            output.stderr.is_empty(),
            "formula rendering wrote to the terminal: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("1 passed"), "{stdout}");
        assert!(!stdout.contains("[ratex-unicode-font]"));
    }
}

#[test]
fn ratex_unicode_formula_uses_glyphs_or_text_fallback() {
    for tex in [r"\text{中文} + x^2", r"\text{质量} = mc^2"] {
        match decode::math_svg(tex, true, 32, [220, 224, 232]) {
            Ok((svg, baseline)) => {
                assert!(svg.contains("<path"));
                assert!(!svg.contains("<text"));
                assert!(baseline > 0.0);

                let image = decode::raster_svg(svg.as_bytes(), 1200, 400, None, false)
                    .unwrap()
                    .to_rgba8();
                assert!(image.pixels().any(|p| p[3] > 0));
                assert!(image.pixels().any(|p| p[3] == 0));
            }

            Err(error) => assert_eq!(error.to_string(), "公式字体不可用"),
        }
    }
}

#[test]
fn ratex_embeds_fraction_integral_and_matrix_glyphs_on_transparent_pixels() {
    for tex in [
        r"E=mc^2",
        r"\frac{-b\pm\sqrt{b^2-4ac}}{2a}",
        r"\int_{-\infty}^{\infty}e^{-x^2}\,dx=\sqrt{\pi}",
        r"\begin{pmatrix}a&b\\c&d\end{pmatrix}",
        r"\begin{aligned}a&=b+c\\d&=e\end{aligned}",
    ] {
        let (svg, baseline) = decode::math_svg(tex, true, 32, [220, 224, 232]).unwrap();
        assert!(svg.contains("<path"), "{tex}");
        assert!(
            !svg.contains("<text"),
            "must not depend on a system math font"
        );
        let image = decode::raster_svg(svg.as_bytes(), 1200, 400, None, false)
            .unwrap()
            .to_rgba8();
        assert!(baseline > 0.0);
        assert!(
            image.pixels().any(|p| p[3] == 0),
            "transparent canvas: {tex}"
        );
        assert!(image.pixels().any(|p| p[3] > 0), "visible glyphs: {tex}");
        assert!(
            image
                .pixels()
                .filter(|p| p[3] > 128)
                .all(|p| p[0] >= 210 && p[2] >= 220)
        );
    }

    assert!(decode::math_svg(r"\unknownOrbitMacro{x}", false, 20, [255; 3]).is_err());
}

#[test]
fn svg_preview_preserves_alpha_and_ignores_external_image_references() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("outside.png");
    image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 255, 0, 255]))
        .save(&secret)
        .unwrap();
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect x="10" width="10" height="10" fill="#ff0000" fill-opacity=".5"/><image href="{}" width="8" height="8"/></svg>"##,
        secret.display()
    );
    let image = decode::raster_svg(svg.as_bytes(), 40, 20, None, false)
        .unwrap()
        .to_rgba8();
    assert_eq!(image.dimensions(), (40, 20));
    assert_eq!(image.get_pixel(0, 0)[3], 0);
    let pixel = image.get_pixel(12, 2);
    assert!(
        pixel[0] > 245 && (120..=135).contains(&pixel[3]),
        "straight RGBA: {pixel:?}"
    );
    assert!(!image.pixels().any(|p| p[1] > 200));
}

#[test]
fn parser_attaches_media_only_to_semantic_math_and_images() {
    let source = r"公式 $x^2$、转义 \$ 和代码 `$literal$` ![示意](plot.svg)";
    let lines = markdown::document(source, 80);
    let media = lines.iter().flat_map(|l| &l.media).collect::<Vec<_>>();
    assert_eq!(media.len(), 2);
    assert_eq!(
        media[0].source,
        Source::Math {
            tex: "x^2".into(),
            display: false
        }
    );
    assert_eq!(media[1].source, Source::Image("plot.svg".into()));
    for line in &lines {
        let text = line.line.to_string();
        for target in &line.media {
            assert!(text.get(target.range.clone()).is_some());
        }
    }

    assert!(
        markdown::document("```markdown\n$x$ ![a](b.png)\n```", 80)
            .iter()
            .all(|l| l.media.is_empty())
    );
    assert!(
        markdown::document("<img src='plot.svg' alt='示意'>", 80)
            .iter()
            .any(|l| l.media.iter().any(|m| matches!(m.source, Source::Image(_))))
    );
    let display = markdown::document("$$\n\\frac{a}{b}\n$$\n后文", 80);
    assert!(display.iter().any(|l| {
        l.media
            .iter()
            .any(|m| matches!(m.source, Source::Math { display: true, .. }))
    }));
    let empty_alt = markdown::document("![](plot.png)", 80);
    assert!(empty_alt[0].line.to_string().contains("图片"));
    assert_eq!(
        empty_alt[0].media[0].source,
        Source::Image("plot.png".into())
    );
}

#[test]
fn inline_graphics_keep_pending_targets_links_and_unicode_word_breaks() {
    let context = context(ProtocolType::Kitty);
    let line = markdown::document(
        "你好 $x^2$ English words [链接](https://example.com) ![待加载](missing.svg)",
        80,
    )
    .remove(0);
    let request = requests(&line, 38).remove(0);
    ready(&context, request.source, request.width, request.height);
    let rows = layout(line, &context, 38);
    assert!(rows.iter().any(|l| !l.graphics.is_empty()));
    assert!(rows.iter().all(|l| l.line.width() <= 38));
    assert!(rows.iter().any(|l| l.line.to_string().contains("English")));
    assert!(
        rows.iter()
            .any(|l| l.links.iter().any(|l| l.url == "https://example.com"))
    );
    let pending = rows.iter().flat_map(|l| &l.media).collect::<Vec<_>>();
    assert!(!pending.is_empty());
    assert!(
        pending
            .iter()
            .all(|m| m.source == Source::Image("missing.svg".into()))
    );
}

#[test]
fn nested_frame_keeps_rails_and_right_edge_on_every_image_row() {
    let context = context(ProtocolType::Kitty);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plot.svg");
    std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="70"><rect width="100" height="70" fill="red"/></svg>"#).unwrap();
    let line = markdown::document(&format!("![示意]({})", path.display()), 80)
        .remove(0)
        .framed(
            vec![
                Span::styled("┊ ", Style::new().fg(ratatui::style::Color::Magenta)),
                Span::raw("│ "),
            ],
            vec![Span::raw(" ".repeat(56)), Span::raw(" │")],
        );
    let request = requests(&line, 64).remove(0);
    let picture = ready(&context, request.source, request.width, request.height);
    let rows = layout(line, &context, 64);
    assert_eq!(
        rows.len(),
        usize::from(picture.size.height),
        "no empty thinking rail before image"
    );
    for line in rows {
        assert!(line.line.to_string().starts_with("┊ │ "));
        assert!(line.line.to_string().ends_with(" │"));
        assert_eq!(line.line.width(), 64);
        assert_eq!(
            line.line.spans[0].style.fg,
            Some(ratatui::style::Color::Magenta)
        );
    }
}

#[test]
fn asynchronous_local_media_reuses_cache_and_math_falls_back_in_halfblocks() {
    let mut context = context(ProtocolType::Halfblocks);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plot.png");
    image::RgbaImage::from_pixel(20, 20, image::Rgba([255, 64, 32, 255]))
        .save(&path)
        .unwrap();
    context.workspace = dir.path().into();
    let source = Source::Image("plot.png".into());
    assert!(context.cached_picture(&source, 20, 12).is_none());
    let first = wait_for(&context, &source, 20, 12);
    assert!(context.changed());
    assert!(!context.changed());
    assert!(Arc::ptr_eq(
        &first,
        &context.picture(&source, 20, 12, false).unwrap()
    ));
    let math = Source::Math {
        tex: "x^2".into(),
        display: false,
    };
    assert!(context.picture(&math, 20, 10, false).is_none());
    assert!(context.error(&math, 20, 10, false).is_some());
    let line = markdown::document("公式 $x^2$", 80).remove(0);
    assert_eq!(layout(line.clone(), &context, 80)[0].line, line.line);
}

#[test]
fn only_visible_media_is_requested_and_remote_images_do_not_require_a_preview() {
    let context = context(ProtocolType::Kitty);
    let items = (0..100)
        .map(|n| TranscriptItem::Assistant(format!("![图片](missing-{n}.png)")))
        .collect::<Vec<_>>();
    let mut layout = TranscriptLayout::default();
    layout.set_media(context.clone());
    layout.update(&items, None, 80);
    assert!(
        context
            .shared
            .as_ref()
            .unwrap()
            .cache
            .lock()
            .unwrap()
            .is_empty()
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 4)).unwrap();
    begin_frame();
    terminal.draw(|f| layout.draw(f, f.area(), 0)).unwrap();
    let cache = context.shared.as_ref().unwrap().cache.lock().unwrap();
    assert!(!cache.is_empty());
    assert!(cache.len() <= 4, "only the visible rows are requested");
    drop(cache);
    // The worker rejects private destinations before network I/O.
    let remote = Source::Image("http://127.0.0.1/image.png".into());
    assert!(context.cached_picture(&remote, 80, 12).is_none());
    assert!(
        !context
            .shared
            .as_ref()
            .unwrap()
            .cache
            .lock()
            .unwrap()
            .keys()
            .any(|k| k.source == remote)
    );
    assert!(context.picture(&remote, 80, 12, false).is_none());
    assert!(
        context
            .shared
            .as_ref()
            .unwrap()
            .cache
            .lock()
            .unwrap()
            .keys()
            .any(|k| k.source == remote)
    );
    clear_layers();
}

#[test]
fn copied_graphics_use_tex_once_without_protocol_bytes_or_thinking_rails() {
    let context = context(ProtocolType::Kitty);
    let source = "先看 $\\frac{a}{b}$ 再看文字";
    let line = crate::tui::render::thinking_linked(markdown::document(source, 80).remove(0));
    let request = requests(&line, 80).remove(0);
    ready(&context, request.source, request.width, request.height);
    let items = [TranscriptItem::Thinking(source.to_owned().into())];
    let mut layout = TranscriptLayout::default();
    layout.set_media(context);
    layout.update(&items, None, 80);
    let area = Rect::new(0, 0, 80, layout.height() as u16);
    let mut selection = Selection::default();
    selection.viewport = area;
    selection
        .handle(&mouse(MouseEventKind::Down(MouseButton::Left), 0, 0))
        .unwrap();
    selection
        .handle(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            79,
            area.height - 1,
        ))
        .unwrap();
    selection.transcript_view(0, &layout);
    let copied = selection.text();
    assert!(copied.contains(source), "{copied:?}");
    assert_eq!(copied.matches(r"\frac{a}{b}").count(), 1);
    assert!(!copied.contains(['┊', '│', '\x1b']));
    assert!(layout.media_at(area, 0, (0, 0)).is_none()); // thinking rail
}

#[test]
fn selecting_one_slice_selects_the_entire_occurrence_and_restores_each_protocol() {
    use crate::tui::theme::THEME;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opaque.png");
    image::RgbaImage::from_pixel(80, 180, image::Rgba([230, 100, 50, 255]))
        .save(&path)
        .unwrap();
    for protocol in [
        ProtocolType::Kitty,
        ProtocolType::Sixel,
        ProtocolType::Iterm2,
        ProtocolType::Halfblocks,
    ] {
        for source in [
            "$$\\frac{a}{b}$$".to_owned(),
            format!("![图片正文]({})", path.display()),
        ] {
            let context = context(protocol);
            let line = markdown::document(&source, 80).remove(0);
            let request = requests(&line, 80).remove(0);
            if matches!(request.source, Source::Math { .. }) && protocol == ProtocolType::Halfblocks
            {
                continue; // This protocol deliberately retains selectable TeX.
            }

            let picture = ready(&context, request.source, request.width, request.height);
            assert!(picture.size.height > 1);
            let mut layout = TranscriptLayout::default();
            layout.set_media(context);
            // Same cache entry, distinct occurrences: only the second is selected.
            layout.update(
                &[TranscriptItem::Assistant(format!(
                    "before\n\n{source}\n\nbetween\n\n{source}\n\nafter"
                ))],
                None,
                80,
            );
            let area = Rect::new(3, 2, 80, layout.height() as u16);
            let mut selection = Selection::default();
            selection.viewport = area;
            let mut terminal = Terminal::new(TestBackend::new(86, area.bottom() + 2)).unwrap();
            begin_frame();
            terminal
                .draw(|f| {
                    selection.transcript_view(0, &layout);
                    layout.draw(f, area, 0);
                    selection.paint(f);
                    paint(f.buffer_mut());
                })
                .unwrap();
            let (top, columns) = (0..layout.height())
                .filter_map(|row| {
                    layout
                        .selection_metadata(row)?
                        .copy
                        .iter()
                        .find(|copy| copy.row == 0)
                        .map(|copy| (row, copy.columns.clone()))
                })
                .nth(1)
                .unwrap();
            let x = area.x + columns.start as u16;
            let y = area.y + top as u16;
            let original = terminal.backend().buffer()[(x, y)].clone();
            for reverse in [false, true] {
                let middle = y + picture.size.height / 2;
                let (a, b) = if reverse { (x + 1, x) } else { (x, x + 1) };
                selection
                    .handle(&mouse(MouseEventKind::Down(MouseButton::Left), a, middle))
                    .unwrap();
                selection
                    .handle(&mouse(MouseEventKind::Drag(MouseButton::Left), b, middle))
                    .unwrap();
                begin_frame();
                terminal
                    .draw(|f| {
                        assert!(selection.draw_frozen(f));
                        for row in y..y + picture.size.height {
                            for column in x..x + picture.size.width {
                                assert_eq!(f.buffer_mut()[(column, row)].bg, THEME.selection);
                            }
                            assert_ne!(f.buffer_mut()[(x - 1, row)].bg, THEME.selection);
                        }

                        LAYER.with(|layer| {
                            for p in &layer.borrow().current {
                                assert_eq!(
                                    p.selected,
                                    usize::from(p.area.y - area.y) - usize::from(p.fragment.row)
                                        == top
                                );
                            }
                        });
                        paint(f.buffer_mut());
                    })
                    .unwrap();
                assert_ne!(terminal.backend().buffer()[(x, y)], original);
                assert_eq!(
                    selection.text().trim(),
                    if source.starts_with("$$") {
                        source.as_str()
                    } else {
                        "图片正文"
                    }
                );
                selection
                    .handle(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
                    .unwrap();
                begin_frame();
                terminal
                    .draw(|f| {
                        layout.draw(f, area, 0);
                        selection.paint(f);
                        LAYER.with(|layer| {
                            assert!(layer.borrow().current.iter().all(|p| !p.selected))
                        });
                        paint(f.buffer_mut());
                    })
                    .unwrap();
                assert_eq!(terminal.backend().buffer()[(x, y)].bg, original.bg);
            }

            clear_layers();
        }
    }
}

#[test]
fn clipped_media_copies_its_source_even_when_the_baseline_is_offscreen() {
    let context = context(ProtocolType::Kitty);
    let source = "$$\\frac{a+b}{c+d}$$";
    let line = crate::tui::render::thinking_linked(markdown::document(source, 80).remove(0));
    let request = requests(&line, 80).remove(0);
    let picture = ready(&context, request.source, request.width, request.height);
    let mut layout = TranscriptLayout::default();
    layout.set_media(context);
    layout.update(
        &[TranscriptItem::Thinking(source.to_owned().into())],
        None,
        80,
    );
    let top = (0..layout.height())
        .find(|&row| {
            layout
                .selection_metadata(row)
                .is_some_and(|m| m.copy.iter().any(|c| c.row == 0))
        })
        .unwrap();
    let offset = top + usize::from(picture.size.height) - 1;
    let copy = &layout.selection_metadata(offset).unwrap().copy[0];
    assert_ne!(copy.row, copy.baseline);
    let area = Rect::new(4, 3, 80, 1);
    let mut selection = Selection::default();
    selection.viewport = area;
    let x = area.x + copy.columns.start as u16;
    let mut terminal = Terminal::new(TestBackend::new(88, 8)).unwrap();
    begin_frame();
    terminal
        .draw(|f| {
            selection.transcript_view(offset, &layout);
            layout.draw(f, area, offset);
            selection.paint(f);
            paint(f.buffer_mut());
        })
        .unwrap();
    selection
        .handle(&mouse(MouseEventKind::Down(MouseButton::Left), x, area.y))
        .unwrap();
    selection
        .handle(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            x + 1,
            area.y,
        ))
        .unwrap();
    begin_frame();
    terminal
        .draw(|f| {
            assert!(selection.draw_frozen(f));
            LAYER.with(|layer| {
                assert_eq!(layer.borrow().current.len(), 1);
                assert!(layer.borrow().current[0].selected);
            });
            paint(f.buffer_mut());
        })
        .unwrap();
    assert_eq!(selection.text().trim(), source);
    assert!(!selection.text().contains(['┊', '\x1b']));
    clear_layers();
}

#[test]
fn selecting_an_inline_formula_preserves_the_surrounding_text_and_other_formula() {
    let context = context(ProtocolType::Kitty);
    let source = "前 $\\frac{a}{b}$ 中 $\\frac{a}{b}$ 后";
    let line = markdown::document(source, 80).remove(0);
    let request = requests(&line, 80).remove(0);
    ready(&context, request.source, request.width, request.height);
    let mut layout = TranscriptLayout::default();
    layout.set_media(context);
    layout.update(&[TranscriptItem::Assistant(source.into())], None, 80);
    let area = Rect::new(2, 1, 80, layout.height() as u16);
    let mut selection = Selection::default();
    selection.viewport = area;
    let mut terminal = Terminal::new(TestBackend::new(84, area.bottom() + 2)).unwrap();
    begin_frame();
    terminal
        .draw(|f| {
            selection.transcript_view(0, &layout);
            layout.draw(f, area, 0);
            selection.paint(f);
            paint(f.buffer_mut());
        })
        .unwrap();
    let (row, copy) = (0..layout.height())
        .find_map(|row| {
            let copy = layout.selection_metadata(row)?.copy.get(1)?;
            (copy.row == 0).then_some((row, copy))
        })
        .unwrap();
    let x = area.x + copy.columns.start as u16;
    let y = area.y + row as u16;
    selection
        .handle(&mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .unwrap();
    selection
        .handle(&mouse(MouseEventKind::Drag(MouseButton::Left), x + 1, y))
        .unwrap();
    begin_frame();
    terminal
        .draw(|f| {
            assert!(selection.draw_frozen(f));
            LAYER.with(|layer| {
                for p in &layer.borrow().current {
                    assert_eq!(p.selected, p.area.x == x);
                }
            });
            paint(f.buffer_mut());
        })
        .unwrap();
    assert_eq!(selection.text().trim(), "$\\frac{a}{b}$");
    clear_layers();
}

#[test]
fn an_outer_selection_does_not_clear_media_selection_in_another_surface() {
    let context = context(ProtocolType::Kitty);
    let picture = ready(
        &context,
        Source::Math {
            tex: "x^2".into(),
            display: false,
        },
        40,
        10,
    );
    let fragment = Fragment {
        picture,
        x: 0,
        row: 0,
    };
    let area = Rect::new(4, 3, 40, 8);
    begin_frame();
    schedule(
        &fragment,
        Rect::new(area.x + 2, area.y + 1, fragment.picture.size.width, 1),
    );
    let blocks = [SelectionBlock {
        rows: 1..1 + usize::from(fragment.picture.size.height),
        columns: 2..2 + usize::from(fragment.picture.size.width),
    }];
    select(area, 0, &blocks);
    select(Rect::default(), 0, &[]);
    select(Rect::new(0, 20, 40, 8), 0, &[]);
    LAYER.with(|layer| assert!(layer.borrow().current[0].selected));
    select(area, 0, &[]);
    LAYER.with(|layer| assert!(!layer.borrow().current[0].selected));
    clear_layers();
}

#[test]
fn compositor_crops_rows_occludes_dialogs_and_uploads_kitty_only_once() {
    let context = context(ProtocolType::Kitty);
    let source = Source::Math {
        tex: r"\frac{a}{b}".into(),
        display: false,
    };
    let picture = ready(&context, source, 40, 10);
    let fragment = Fragment {
        picture,
        x: 0,
        row: 0,
    };
    let area = Rect::new(2, 1, fragment.picture.size.width, 1);
    begin_frame();
    schedule(&fragment, area);
    schedule(&fragment, area);
    let mut buffer = Buffer::empty(Rect::new(0, 0, 50, 10));
    paint(&mut buffer);
    assert!(
        buffer[(2, 1)].symbol().contains("a=T"),
        "first Kitty transmission is preserved"
    );
    begin_frame();
    schedule(
        &Fragment {
            row: 1,
            ..fragment.clone()
        },
        area,
    );
    paint(&mut buffer);
    assert!(!buffer[(2, 1)].symbol().contains("a=T"));
    begin_frame();
    schedule(&fragment, area);
    occlude(area);
    let mut buffer = Buffer::empty(buffer.area);
    paint(&mut buffer);
    assert!(buffer.content.iter().all(|c| c.symbol() == " "));
    clear_layers();
}

#[test]
fn preview_consumes_input_and_closes_without_touching_the_composer() {
    let mut preview = Preview::new(
        context(ProtocolType::Halfblocks),
        Source::Math {
            tex: "x^2".into(),
            display: false,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
    terminal.draw(|f| preview.draw(f)).unwrap();
    let buffer = terminal.backend().buffer();
    let (x, y) = (0..30)
        .find_map(|y| {
            (0..90).find_map(|x| {
                (buffer[(x, y)].symbol() == crate::tui::icons::CLOSE_THICK).then_some((x, y))
            })
        })
        .unwrap();

    preview.handle(&mouse(MouseEventKind::Moved, x - 1, y));
    terminal.draw(|f| preview.draw(f)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(x - 1, y)].bg, buffer[(x + 1, y)].bg);
    assert_ne!(buffer[(x - 1, y)].bg, ratatui::style::Color::Reset);
    assert_eq!(
        preview.handle(&mouse(MouseEventKind::Down(MouseButton::Left), x + 1, y)),
        (true, true)
    );
    assert_eq!(preview.handle(&Event::Paste("draft".into())), (true, false));
    assert_eq!(
        preview.handle(&Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        ))),
        (true, false)
    );
    assert_eq!(
        preview.handle(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
        (true, true)
    );
    for (w, h) in [(1, 1), (4, 3), (40, 12)] {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| preview.draw(f)).unwrap();
    }

    clear_layers();
}

#[test]
fn scrolling_a_picture_clips_slices_and_keeps_the_rest_of_the_frame_clean() {
    let context = context(ProtocolType::Kitty);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tall.svg");
    std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="200"><rect width="80" height="200" fill="red"/></svg>"#).unwrap();
    let text = format!("![图]({})", path.display());
    let line = markdown::document(&text, 80).remove(0);
    let request = requests(&line, 80).remove(0);
    let picture = ready(
        &context,
        request.source.clone(),
        request.width,
        request.height,
    );

    assert!(picture.size.height > 4);
    let mut layout = TranscriptLayout::default();
    layout.set_media(context);
    layout.update(&[TranscriptItem::Assistant(text)], None, 80);
    let area = Rect::new(4, 2, 80, 4);
    let mut terminal = Terminal::new(TestBackend::new(90, 10)).unwrap();
    begin_frame();
    terminal
        .draw(|f| {
            layout.draw(f, area, 3);
            paint(f.buffer_mut());
        })
        .unwrap();
    assert_eq!(
        layout.media_at(
            area,
            3,
            (area.x + (area.width - picture.size.width) / 2, area.y)
        ),
        Some(request.source)
    );
    let buffer = terminal.backend().buffer();
    assert!(buffer.content.iter().enumerate().all(|(i, c)| {
        area.contains(((i % 90) as u16, (i / 90) as u16).into()) || c.symbol() == " "
    }));
    LAYER.with(|l| {
        let layer = l.borrow();
        assert_eq!(layer.current[0].fragment.row, 3);
        assert_eq!(layer.current.len(), 4);
    });
    clear_layers();
}

#[test]
fn every_formula_slice_keeps_its_allocated_columns_when_scrolled_and_resized() {
    for width in [32, 80, 132] {
        for display in [false, true] {
            let context = context(ProtocolType::Kitty);
            let tex = r"\frac{a+b}{c+d}";
            let source = if display {
                format!("$$\n{tex}\n$$")
            } else {
                format!("前 ${tex}$ 后")
            };
            let line = markdown::document(&source, width).remove(0);
            let request = requests(&line, width).remove(0);
            let picture = ready(&context, request.source, request.width, request.height);
            assert!(picture.size.height > 1);
            let column = if display {
                2 + (width - 4 - picture.size.width) / 2
            } else {
                5
            };
            let mut layout = TranscriptLayout::default();
            layout.set_media(context);
            layout.update(&[TranscriptItem::Assistant(source)], None, width);
            let area = Rect::new(3, 2, width, 12);
            let mut terminal = Terminal::new(TestBackend::new(width + 6, 16)).unwrap();
            for offset in [0, 1] {
                begin_frame();
                terminal
                    .draw(|f| {
                        layout.draw(f, area, offset);
                        paint(f.buffer_mut());
                    })
                    .unwrap();
                LAYER.with(|layer| {
                    let layer = layer.borrow();
                    assert_eq!(
                        layer.current.len(),
                        usize::from(picture.size.height).saturating_sub(offset)
                    );
                    for placement in &layer.current {
                        assert_eq!(
                            placement.area.x,
                            area.x + column,
                            "display={display}, width={width}, row={}",
                            placement.fragment.row
                        );
                        assert_eq!(placement.area.width, picture.size.width);
                        assert!(area.contains((placement.area.x, placement.area.y).into()));
                    }
                });
            }

            clear_layers();
        }
    }
}

#[test]
fn remote_images_keep_the_source_or_outer_link_on_every_picture_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    image::RgbaImage::from_pixel(40, 60, image::Rgba([230, 100, 50, 255]))
        .save(&path)
        .unwrap();
    let url = "https://orbit.example/thumbnail.png";
    let destination = "https://orbit.example/document";
    for (source, target) in [
        (format!("![占位图片]({url} \"随机占位图\")"), url),
        (
            format!("<img src='{url}' alt='HTML 图片' width='400'>"),
            url,
        ),
        (format!("[![示意]({url})]({destination})"), destination),
        (
            format!("<a href='{destination}'><img src='{url}' alt='示意'></a>"),
            destination,
        ),
    ] {
        let context = context(ProtocolType::Kitty);
        let line = markdown::document(&source, 80).remove(0);
        assert_eq!(line.media.len(), 1);
        assert_eq!(line.links[0].url, target);
        let request = requests(&line, 80).remove(0);
        let mut key = Key {
            network: None,
            files: None,
            source: Source::Image(path.to_string_lossy().into_owned()),
            workspace: dir.path().into(),
            width: request.width,
            height: request.height,
            large: false,
            view: None,
            native: false,
        };
        let shared = context.shared.as_ref().unwrap();
        let mut picture = decode::Decoder::default()
            .prepare(&key, &shared.picker)
            .unwrap();
        picture.source = request.source.clone();
        let height = picture.size.height;
        let column = (80 - picture.size.width) / 2;
        key.source = request.source.clone();
        key.network = Some(context.network.clone());
        key.workspace = context.workspace.clone();
        shared.cache.lock().unwrap().insert(
            key,
            Cached {
                value: Value::Ready(Arc::new(picture)),
                touched: 0,
            },
        );
        let mut layout = TranscriptLayout::default();
        layout.set_media(context);
        layout.update(&[TranscriptItem::Assistant(source)], None, 80);
        let area = Rect::new(0, 0, 80, 20);
        for row in 0..height {
            assert_eq!(layout.link_at(area, 0, (column, row)), Some(target));
            assert_eq!(
                layout.media_at(area, 0, (column, row)),
                Some(request.source.clone())
            );
        }
    }
}

#[test]
fn images_render_without_clicking_and_only_wrapped_links_show_navigation_hints() {
    use crate::tui::app::{ToolRow, ToolStatus};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    image::RgbaImage::from_pixel(400, 600, image::Rgba([230, 100, 50, 255]))
        .save(&path)
        .unwrap();
    let destination = "https://orbit.example/document";

    let image = format!("![示意]({})", path.display());
    let mut thinking = crate::tui::app::ThinkingRow::from(image.clone());
    thinking.finished = true;
    for (item, linked, expanded) in [
        (TranscriptItem::Assistant(image.clone()), false, false),
        (
            TranscriptItem::UserWithImages {
                text: "**原样显示的文字**".into(),
                images: vec![orbit_protocol::MessageImage {
                    name: "clipboard.png".into(),
                    path: path.to_string_lossy().into_owned(),
                }],
            },
            false,
            false,
        ),
        (
            TranscriptItem::UserWithImages {
                text: String::new(),
                images: vec![orbit_protocol::MessageImage {
                    name: "截图.png".into(),
                    path: path.to_string_lossy().into_owned(),
                }],
            },
            false,
            false,
        ),
        (
            TranscriptItem::Assistant(format!("<img src='{}' alt='示意'>", path.display())),
            false,
            false,
        ),
        (
            TranscriptItem::Assistant(format!("[![示意]({})]({destination})", path.display())),
            true,
            false,
        ),
        (
            TranscriptItem::Assistant(format!(
                "<a href='{destination}'><img src='{}' alt='示意'></a>",
                path.display()
            )),
            true,
            false,
        ),
        (TranscriptItem::Thinking(thinking), false, true),
        (
            TranscriptItem::Tool(ToolRow {
                id: "read-image".into(),
                name: "read_image".into(),
                arguments: serde_json::json!({"path": path}).to_string(),
                output: serde_json::json!({"path": path}).to_string(),
                status: ToolStatus::Finished,
            }),
            false,
            true,
        ),
    ] {
        let thumbnail = matches!(&item, TranscriptItem::UserWithImages { .. });
        let mut state = UiState::new("mock");
        state.media = context(ProtocolType::Kitty);
        state.transcript.push(item);
        if expanded {
            state.expanded.insert(0);
        }
        state.input.insert("未发送的草稿");
        let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            draw_ui(&mut state, &mut terminal);
            if terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol().contains('\u{10eeee}'))
            {
                break;
            }

            assert!(Instant::now() < deadline, "image did not render inline");
            std::thread::sleep(Duration::from_millis(5));
        }

        let area = state.selection.viewport;
        let point = (area.y..area.bottom())
            .find_map(|y| {
                (area.x..area.right()).find_map(|x| {
                    state
                        .transcript_layout
                        .media_at(area, state.max_scroll, (x, y))
                        .is_some()
                        .then_some((x, y))
                })
            })
            .expect("inline image has a hit area");
        if thumbnail {
            let bounds = state
                .transcript_layout
                .picture_area_at(area, state.max_scroll, point)
                .unwrap();
            assert!(bounds.width <= 32, "thumbnail width: {}", bounds.width);
            assert!(bounds.height <= 6, "thumbnail height: {}", bounds.height);
        }
        let target = state
            .transcript_layout
            .link_at(area, state.max_scroll, point);
        assert_eq!(target, linked.then_some(destination));
        crate::tui::render::handle_link_hover(
            &mut state,
            &mouse(MouseEventKind::Moved, point.0, point.1),
        );
        assert_eq!(state.link_hovered, linked.then_some(point));
        draw_ui(&mut state, &mut terminal);
        let tooltip = state.link_tooltip_area;
        let mut shown = String::new();
        let mut x = tooltip.x;
        while x < tooltip.right() {
            let cell = &terminal.backend().buffer()[(x, tooltip.y)];
            shown.push_str(cell.symbol());
            x += cell.cell_width().max(1);
        }

        assert_eq!(
            shown.replace(' ', "").contains("Ctrl+Click跳转"),
            linked,
            "{shown:?}"
        );
        assert!(!shown.contains("单击预览"));

        state
            .selection
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                point.0,
                point.1,
            ))
            .unwrap();
        assert!(state.selection.dragging());
        let release = mouse(MouseEventKind::Up(MouseButton::Left), point.0, point.1);
        let clicked = state.selection.click(&release).unwrap();
        assert!(crate::tui::toggle_transcript_at(&mut state, clicked));
        assert!(!state.selection.dragging());
        assert!(!state.selection.active());
        assert!(state.media_preview.is_some());
        draw_ui(&mut state, &mut terminal);
        assert!(state.ime_cursor.is_none());
        assert_eq!(
            state
                .media_preview
                .as_mut()
                .unwrap()
                .handle(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
            (true, true)
        );
        state.media_preview = None;
        draw_ui(&mut state, &mut terminal);
        crate::tui::render::handle_link_hover(
            &mut state,
            &mouse(MouseEventKind::Moved, point.0, point.1),
        );
        assert_eq!(state.link_hovered, linked.then_some(point));
        assert_eq!(state.input.text, "未发送的草稿");
        if expanded {
            let bounds = state
                .transcript_layout
                .picture_area_at(area, state.max_scroll, point)
                .unwrap();
            assert!(bounds.height > 1);
            for y in bounds.y..bounds.bottom() {
                for x in [area.x + 2, area.right() - 3] {
                    assert!(
                        state
                            .transcript_layout
                            .media_at(area, state.max_scroll, (x, y))
                            .is_none()
                    );
                    assert_eq!(
                        state.transcript_layout.disclosure_at(
                            &state.transcript,
                            area,
                            state.max_scroll,
                            (x, y),
                        ),
                        Some(0),
                        "blank space beside every image row must remain collapsible"
                    );
                }
            }
            assert!(crate::tui::toggle_transcript_at(
                &mut state,
                (area.right() - 3, bounds.bottom() - 1),
            ));
            assert!(!state.expanded.contains(&0));
            assert!(state.media_preview.is_none());
        }
        clear_layers();
    }
}

#[test]
fn linked_image_hints_stay_outside_pixels_while_the_pointer_moves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    image::RgbaImage::from_pixel(600, 300, image::Rgba([230, 100, 50, 255]))
        .save(&path)
        .unwrap();
    let target = "https://orbit.example/document";
    let image = format!("[![示意]({})]({target})", path.display());
    let documents = [
        format!("before\n\n{image}\n\nbetween\n\n{image}\n\nafter"),
        format!(
            "<table><tr><td><a href='{target}'><img src='{}'></a></td><td><a href='{target}'><img src='{}'></a></td></tr></table>",
            path.display(),
            path.display()
        ),
    ];

    for protocol in [
        ProtocolType::Kitty,
        ProtocolType::Sixel,
        ProtocolType::Iterm2,
        ProtocolType::Halfblocks,
    ] {
        for width in [34, 80, 140] {
            for document in &documents {
                let mut state = UiState::new("mock");
                state.media = context(protocol);
                state
                    .transcript
                    .push(TranscriptItem::Assistant(document.clone()));
                let mut terminal = Terminal::new(TestBackend::new(width, 48)).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                let origins = loop {
                    draw_ui(&mut state, &mut terminal);
                    let origins: Vec<_> = LAYER.with(|layer| {
                        layer
                            .borrow()
                            .current
                            .iter()
                            .filter(|placement| placement.fragment.row == 0)
                            .map(|placement| (placement.area.x, placement.area.y))
                            .collect()
                    });
                    if origins.len() == 2 {
                        break origins;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "pictures did not render: {protocol:?}, {width}, {document}"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                };
                let placements = || {
                    LAYER.with(|layer| {
                        layer
                            .borrow()
                            .current
                            .iter()
                            .map(|placement| (placement.area, placement.fragment.row))
                            .collect::<Vec<_>>()
                    })
                };
                let original = placements();
                let viewport = state.selection.viewport;
                let pictures: Vec<_> = origins
                    .iter()
                    .map(|&point| {
                        state
                            .transcript_layout
                            .picture_area_at(viewport, state.max_scroll, point)
                            .unwrap()
                    })
                    .collect();

                assert_ne!(pictures[0], pictures[1]);

                for picture in &pictures {
                    let mut tooltip = None;
                    for (index, point) in [
                        (picture.x, picture.y),
                        (
                            picture.x + picture.width / 2,
                            picture.y + picture.height / 2,
                        ),
                        (picture.right() - 1, picture.bottom() - 1),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        assert_eq!(
                            state
                                .transcript_layout
                                .link_at(viewport, state.max_scroll, point),
                            Some(target)
                        );
                        assert_eq!(
                            state.transcript_layout.picture_area_at(
                                viewport,
                                state.max_scroll,
                                point
                            ),
                            Some(*picture)
                        );
                        assert_eq!(
                            crate::tui::render::handle_link_hover(
                                &mut state,
                                &mouse(MouseEventKind::Moved, point.0, point.1)
                            ),
                            Some(index == 0)
                        );
                        draw_ui(&mut state, &mut terminal);
                        let area = state.link_tooltip_area;
                        assert!(
                            !area.is_empty(),
                            "no hint: {protocol:?}, {width}, {document}"
                        );
                        assert!(
                            pictures
                                .iter()
                                .all(|picture| area.intersection(*picture).is_empty())
                        );
                        assert_eq!(area.y, picture.bottom());
                        let mut column = area.x;
                        while column < area.right() {
                            let cell = &terminal.backend().buffer()[(column, area.y)];
                            assert_eq!(cell.fg, crate::tui::theme::THEME.overlay_foreground);
                            assert_eq!(cell.bg, crate::tui::theme::THEME.overlay_background);
                            column += cell.cell_width().max(1);
                        }

                        if let Some(tooltip) = tooltip {
                            assert_eq!(area, tooltip);
                        }
                        tooltip = Some(area);
                        assert_eq!(placements(), original, "hover erased an image slice");
                    }
                }

                assert_eq!(
                    crate::tui::render::handle_link_hover(
                        &mut state,
                        &mouse(MouseEventKind::Moved, 0, 0)
                    ),
                    Some(true)
                );
                draw_ui(&mut state, &mut terminal);
                assert!(state.link_tooltip_area.is_empty());
                assert_eq!(placements(), original);
                clear_layers();
            }
        }
    }
}

#[test]
fn cropped_image_hover_uses_current_bounds_after_scroll_and_resize() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tall.png");
    image::RgbaImage::from_pixel(600, 600, image::Rgba([40, 150, 220, 255]))
        .save(&path)
        .unwrap();
    let source = format!(
        "[![图]({})](https://orbit.example/document)",
        path.display()
    );
    let mut state = UiState::new("mock");
    state.media = context(ProtocolType::Kitty);
    state
        .transcript
        .push(TranscriptItem::Assistant(source.clone()));
    let mut terminal = Terminal::new(TestBackend::new(160, 14)).unwrap();
    let draw = |state: &mut UiState, terminal: &mut Terminal<TestBackend>, area| {
        begin_frame();
        terminal
            .draw(|frame| {
                crate::tui::render::draw_transcript_in(frame, state, area, false);
                state.selection.paint(frame);
                paint(frame.buffer_mut());
            })
            .unwrap();
    };

    for width in [28, 140, 80] {
        let request = requests(&markdown::document(&source, width).remove(0), width).remove(0);
        ready(&state.media, request.source, request.width, request.height);
        let area = Rect::new(4, 4, width, 3);
        draw(&mut state, &mut terminal, area);
        state.follow_bottom = false;
        state.scroll = state.max_scroll.saturating_sub(2);
        draw(&mut state, &mut terminal, area);
        let origins: Vec<_> = LAYER.with(|layer| {
            layer
                .borrow()
                .current
                .iter()
                .map(|placement| (placement.area, placement.fragment.row))
                .collect()
        });

        assert_eq!(origins.len(), 3);
        assert_eq!(origins[0].1, 2);
        let point = (origins[0].0.x, area.y);
        let picture = state
            .transcript_layout
            .picture_area_at(area, 2, point)
            .unwrap();
        assert_eq!(picture.y, area.y);
        assert_eq!(picture.height, area.height);
        for y in area.y..area.bottom() {
            assert_eq!(
                state
                    .transcript_layout
                    .picture_area_at(area, 2, (point.0, y)),
                Some(picture)
            );
            crate::tui::render::handle_link_hover(
                &mut state,
                &mouse(MouseEventKind::Moved, point.0, y),
            );
            draw(&mut state, &mut terminal, area);
            assert!(state.link_tooltip_area.intersection(picture).is_empty());
            LAYER.with(|layer| {
                assert_eq!(
                    layer
                        .borrow()
                        .current
                        .iter()
                        .map(|placement| (placement.area, placement.fragment.row))
                        .collect::<Vec<_>>(),
                    origins
                )
            });
        }

        state.scroll = state.max_scroll.saturating_sub(4);
        draw(&mut state, &mut terminal, area);
        let next = state
            .transcript_layout
            .picture_area_at(area, 4, point)
            .unwrap();
        assert_eq!(next, picture);
        assert!(state.link_tooltip_area.intersection(next).is_empty());
        LAYER.with(|layer| {
            assert_eq!(layer.borrow().current.len(), 3);
            assert_eq!(layer.borrow().current[0].fragment.row, 4);
        });
        clear_layers();
    }
}

#[test]
fn image_alt_urls_are_not_implicit_navigation_targets() {
    let label = "https://orbit.example/alt";
    for source in [
        format!("![{label}](image.png)"),
        format!("<img src='image.png' alt='{label}'>"),
        format!("![{label}][image]\n\n[image]: image.png"),
    ] {
        let lines = markdown::document(&source, 80);
        let image = lines.iter().find(|line| !line.media.is_empty()).unwrap();
        assert!(image.links.is_empty(), "{source}");
        assert_eq!(image.media[0].source, Source::Image("image.png".into()));
    }
}

#[test]
fn table_formulas_share_cell_layout_keep_borders_links_and_copy_complete_tex() {
    let tex = [r"e^{i\pi}", r"\frac{a}{b}"];
    let sources = [
        format!(
            "| 左对齐 | 居中 | 右对齐 |\n| :--- | :---: | ---: |\n| 中文 | [链接](https://orbit.example/docs) | ${}$ |\n| `code` | ${}$ | 42 |\n",
            tex[0], tex[1]
        ),
        format!(
            "<table><tr><th>左</th><th>右</th></tr><tr><td>中文</td><td align='right'>${}$</td></tr><tr><td colspan='2' align='center'>[链接](https://orbit.example/docs) ${}$</td></tr></table>",
            tex[0], tex[1]
        ),
    ];

    for protocol in [
        ProtocolType::Kitty,
        ProtocolType::Sixel,
        ProtocolType::Iterm2,
    ] {
        for source in &sources {
            for width in [12, 34, 80] {
                let context = context(protocol);
                let document = markdown::document(source, width);
                let pending = document
                    .iter()
                    .cloned()
                    .flat_map(|line| layout(line, &context, width))
                    .collect::<Vec<_>>();
                let mut requested = std::collections::HashSet::new();
                for request in pending.iter().flat_map(|line| &line.media_requests) {
                    if requested.insert((request.source.clone(), request.width, request.height)) {
                        ready(
                            &context,
                            request.source.clone(),
                            request.width,
                            request.height,
                        );
                    }
                }

                assert_eq!(requested.len(), 2, "width={width}, {source}");
                let rendered = document
                    .into_iter()
                    .flat_map(|line| layout(line, &context, width))
                    .collect::<Vec<_>>();
                let text = rendered
                    .iter()
                    .map(LinkedLine::text)
                    .collect::<Vec<_>>()
                    .join("\n");

                assert!(rendered.iter().any(|line| !line.graphics.is_empty()));
                assert!(!text.contains(tex[0]) && !text.contains(tex[1]), "{text}");
                assert!(
                    rendered
                        .iter()
                        .all(|line| line.line.width() <= usize::from(width)),
                    "{text}"
                );
                assert!(
                    rendered
                        .iter()
                        .flat_map(|line| &line.links)
                        .any(|link| link.url == "https://orbit.example/docs")
                );
                for line in &rendered {
                    for graphic in &line.graphics {
                        let text = line.text();
                        let decorations = line
                            .decorations
                            .iter()
                            .map(|range| text[..range.start].width()..text[..range.end].width())
                            .collect::<Vec<_>>();

                        for column in usize::from(graphic.x)
                            ..usize::from(graphic.x + graphic.picture.size.width)
                        {
                            assert!(
                                !decorations.iter().any(|range| range.contains(&column)),
                                "formula overlaps table border"
                            );
                        }
                    }
                }

                let mut transcript = TranscriptLayout::default();
                transcript.set_media(context);
                transcript.update(&[TranscriptItem::Assistant(source.clone())], None, width);
                let mut selection = Selection::default();
                let area = Rect::new(0, 0, width, transcript.height() as u16);
                selection.viewport = area;
                selection
                    .handle(&mouse(MouseEventKind::Down(MouseButton::Left), 0, 0))
                    .unwrap();
                selection
                    .handle(&mouse(
                        MouseEventKind::Drag(MouseButton::Left),
                        width - 1,
                        area.bottom() - 1,
                    ))
                    .unwrap();
                selection.transcript_view(0, &transcript);
                let copied = selection.text();
                for formula in tex {
                    assert_eq!(copied.matches(formula).count(), 1, "{copied:?}");
                }
                assert!(!copied.contains(['┌', '│', '\x1b']));
            }
        }
    }
}

#[test]
fn table_math_decodes_automatically_and_only_requests_visible_rows() {
    let mut state = UiState::new("mock");
    state.media = context(ProtocolType::Kitty);
    let mut source = "| 序号 | 公式 |\n| --- | ---: |\n".to_owned();
    for row in 0..80 {
        source.push_str(&format!("| {row} | $x_{{{row}}}$ |\n"));
    }
    state.transcript.push(TranscriptItem::Assistant(source));
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        draw_ui(&mut state, &mut terminal);
        if terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol().contains('\u{10eeee}'))
        {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "visible table math did not decode"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let shared = state.media.shared.as_ref().unwrap();
    let cache = shared.cache.lock().unwrap();
    assert!(!cache.is_empty());
    assert!(cache.len() < usize::from(state.selection.viewport.height));
    assert!(
        cache
            .keys()
            .all(|key| key.width < state.selection.viewport.width)
    );
    clear_layers();
}

#[test]
fn attachment_chips_decode_tiny_thumbnails_without_showing_filenames() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private-filename.png");
    image::RgbaImage::from_pixel(80, 60, image::Rgba([230, 100, 50, 255]))
        .save(&path)
        .unwrap();
    let mut state = UiState::new("mock");
    state.media = context(ProtocolType::Kitty);
    state
        .attachments
        .push(orbit_model::types::Attachment::Image {
            name: "private-filename.png".into(),
            mime_type: "image/png".into(),
            path,
        });
    state.input.insert("说明这张图片");
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        draw_ui(&mut state, &mut terminal);
        let chip = state.attachment_areas[0];
        if (chip.y..chip.bottom()).any(|y| {
            (chip.x..chip.right()).any(|x| {
                terminal.backend().buffer()[(x, y)]
                    .symbol()
                    .contains('\u{10eeee}')
            })
        }) {
            break;
        }

        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }

    let chip = state.attachment_areas[0];
    assert_eq!(chip.height, 2);
    assert!(chip.width <= 12);
    assert!(state.input_area.y >= chip.bottom());
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in buffer.area.y..buffer.area.bottom() {
        let mut x = buffer.area.x;
        while x < buffer.area.right() {
            let cell = &buffer[(x, y)];
            text.push_str(cell.symbol());
            x += cell.cell_width().max(1);
        }

        text.push('\n');
    }

    assert!(text.contains("图片 1"));
    assert!(!text.contains("private-filename"));
    assert!(text.contains("说明这张图片"));
    let shared = state.media.shared.as_ref().unwrap();
    assert!(
        shared
            .cache
            .lock()
            .unwrap()
            .keys()
            .all(|key| key.width == 4 && key.height == 2)
    );
    clear_layers();
}

/// Generates a real export for visual inspection, independently of the terminal.
#[test]
#[ignore = "writes formula gallery to /tmp for manual visual inspection"]
fn export_formula_gallery() {
    let examples = [
        r"E=mc^2",
        r"\frac{-b\pm\sqrt{b^2-4ac}}{2a}",
        r"\int_{-\infty}^{\infty}e^{-x^2}\,dx=\sqrt{\pi}",
        r"\begin{pmatrix}a&b\\c&d\end{pmatrix}",
    ];
    let mut gallery = image::RgbaImage::from_pixel(850, 460, image::Rgba([13, 18, 23, 255]));
    for (n, tex) in examples.iter().enumerate() {
        let (svg, _) = decode::math_svg(tex, true, 64, [220, 224, 232]).unwrap();
        let picture = decode::raster_svg(svg.as_bytes(), 800, 108, None, false).unwrap();
        image::imageops::overlay(&mut gallery, &picture.to_rgba8(), 24, (n * 115 + 4) as i64);
        std::fs::write(format!("/tmp/orbit-formula-{n}.svg"), svg).unwrap();
    }

    gallery.save("/tmp/orbit-ratex-gallery.png").unwrap();
}

#[test]
fn thinking_formulas_keep_combined_rails_on_every_wrapped_and_graphic_row() {
    let source = r"Analyzing prefix trees for carry calculation with $G_{HL}=G_H+P_H G_L$ and $P_{HL}=P_H P_L$. Considering $\frac{C_{in}}{2}$ as a special case before scripting the circuit for verification.";
    for width in [32, 64, 100] {
        for prefix in ["  ┊ ", "  ┊ │ "] {
            let context = context(ProtocolType::Kitty);
            let mut line = markdown::document(source, width).remove(0);
            line.strip_body_margin();
            let line = line.framed(
                vec![Span::styled(
                    prefix,
                    Style::new().fg(crate::tui::theme::THEME.thinking_rail),
                )],
                Vec::new(),
            );
            for request in requests(&line, width) {
                ready(&context, request.source, request.width, request.height);
            }
            let rows = layout(line, &context, width);
            assert!(rows.len() > 2);
            assert!(rows.iter().any(|line| !line.graphics.is_empty()));
            for row in rows {
                assert!(
                    row.text().starts_with(prefix),
                    "width={width}: {:?}",
                    row.text()
                );
                assert!(row.line.width() <= usize::from(width));
                for graphic in &row.graphics {
                    assert!(usize::from(graphic.x) >= prefix.width());
                    assert!(graphic.x + graphic.picture.size.width <= width);
                }
            }
        }
    }
}

#[test]
fn identical_paths_in_different_workspaces_keep_separate_media_layout_caches() {
    let (first_requests, _) = tokio::sync::mpsc::channel(1);
    let (second_requests, _) = tokio::sync::mpsc::channel(1);
    let first = Context {
        files: Some(crate::workspace::Files {
            requests: std::sync::Arc::new(first_requests),
            session: "same-id".into(),
        }),
        ..Context::default()
    };
    let mut second = first.clone();
    second.files = Some(crate::workspace::Files {
        requests: std::sync::Arc::new(second_requests),
        session: "same-id".into(),
    });
    assert!(!first.same_cache(&second));
    assert!(first.same_cache(&first.clone()));
}
