//! Isolated native TextView reflow probe with generated content only.
use gpui_kit::component::{
    ActiveTheme, Theme,
    text::{TextView, TextViewState},
};
use gpui_kit::*;
use std::{path::PathBuf, time::Duration};

struct Reflow {
    text: Entity<TextViewState>,
}

impl Render for Reflow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TextView::new(&self.text)
                    .w_full()
                    .min_w_0(),
            )
    }
}

fn main() -> anyhow::Result<()> {
    let captures = std::env::args_os()
        .nth(1)
        .map(PathBuf::from);
    if let Some(directory) = &captures {
        std::fs::create_dir_all(directory)?;
    }
    let source = (1..=5)
        .map(|index| {
            format!(
                "{}完成：`.unit_{index}`、`.segment_{index}`、`.record_{index}`，其余合成内容保持原样。",
                "生成样本".repeat(index)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let theme = Theme::global_mut(cx);
        theme.font_size = px(20.);
        theme.font_family = "Noto Sans CJK SC".into();
        theme.mono_font_family = "Noto Sans Mono".into();
        Theme::sync_base(cx);
        let (handle, _) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1_000.), px(850.)),
                ))),
                ..Default::default()
            },
            cx,
            |_, cx| {
                cx.new(|cx| Reflow {
                    text: cx.new(|cx| TextViewState::markdown(&source, cx)),
                })
            },
        )
        .expect("open isolated reflow window");
        cx.spawn(async move |cx| {
            for (stage, width) in [1_000., 380., 780., 380.]
                .into_iter()
                .enumerate()
            {
                handle
                    .update(cx, |_, window, _| window.resize(size(px(width), px(850.))))
                    .expect("resize isolated reflow window");
                cx.background_executor()
                    .timer(Duration::from_secs(2))
                    .await;
                if cfg!(target_os = "linux")
                    && let Some(directory) = &captures
                {
                    let path = directory.join(format!("stage-{stage}.png"));
                    let display = std::env::var("DISPLAY").expect("isolated Xvfb display");
                    let status = std::process::Command::new("ffmpeg")
                        .args([
                            "-nostdin",
                            "-loglevel",
                            "error",
                            "-y",
                            "-f",
                            "x11grab",
                            "-draw_mouse",
                            "0",
                            "-video_size",
                            "1280x900",
                            "-i",
                        ])
                        .arg(display)
                        .args(["-frames:v", "1"])
                        .arg(path)
                        .status()
                        .expect("capture isolated reflow window");
                    assert!(status.success(), "capture failed");
                }
            }
            if cfg!(target_os = "linux")
                && let Some(directory) = &captures
            {
                let first = image::open(directory.join("stage-1.png"))
                    .expect("read first narrow capture")
                    .to_rgba8();
                let repeated = image::open(directory.join("stage-3.png"))
                    .expect("read repeated narrow capture")
                    .to_rgba8();
                assert!(
                    first == repeated,
                    "narrow layout changed after resizing back"
                );
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    Ok(())
}
