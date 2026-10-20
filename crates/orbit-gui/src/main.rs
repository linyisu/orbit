//! GPUI Kit desktop shell; workspace services remain behind orbit-client.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod accounts;
mod appearance;
mod assets;
mod backend;
mod conversation;
mod dialog;
mod execution;
mod file_preview;
mod image_http;
mod image_preview;
mod keymap;
mod math;
mod preferences;
mod speech;
mod theme;
mod usage;
mod workbench;
mod workspace;

use clap::Parser;
use gpui_kit::component::Theme;
use gpui_kit::*;
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    if orbit_client::interaction::is_askpass() {
        return orbit_client::interaction::askpass();
    }
    orbit_client::register_server_bundle(include_bytes!(env!("ORBIT_EMBEDDED_SERVER")))?;
    orbit_client::register_remote_server_bundle(include_bytes!(env!(
        "ORBIT_EMBEDDED_REMOTE_SERVER"
    )))?;
    let mut options = backend::Options::parse();
    options.resolve_paths()?;
    let smoke_test = options.smoke_test.take();
    let smoke_account_edit = options.smoke_account_edit;
    options.smoke_account_edit = false;
    if smoke_test.is_some() {
        assets::embedded_icon_count()?;
    }
    let shutdown_report = smoke_test.clone();
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let application_runtime = runtime.clone();
    let preferences = preferences::Preferences::load(options.preferences.as_deref(), &runtime)?;
    let (loaded, writer) = workbench::storage::Storage::load(&options)?;
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .with_http_client(Arc::new(image_http::ImageHttpClient::new(
            runtime.handle().clone(),
        )?))
        .run(move |cx| {
            cx.set_app_identity("com.orbit.desktop", "Orbit");
            gpui_kit::init(cx);
            image_preview::init(cx);
            let mode = preferences.mode(cx.window_appearance());
            cx.set_global(preferences);
            gpui_kit::component::set_locale("zh-CN");
            theme::install(cx);
            Theme::change(mode, None, cx);
            preferences::Preferences::apply_font(cx);
            workspace::bind_keys(cx);
            conversation::bind_keys(cx);
            // Window-system geometry is the physical boundary; content uses rem.
            let bounds = if cfg!(feature = "test-support")
                && smoke_test.is_some()
                && std::env::var_os("ORBIT_GUI_DETAILS_PROBE").is_some()
            {
                Bounds::new(point(px(0.), px(0.)), size(px(1180.), px(800.)))
            } else {
                Bounds::centered(None, size(px(1180.), px(800.)), cx)
            };
            let result = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(800.), px(560.))),
                    app_id: Some("orbit".into()),
                    ..gpui_kit::component::TitleBar::window_options()
                },
                cx,
                |window, cx| {
                    window.set_window_title("Orbit");
                    cx.new(|cx| {
                        workbench::Workbench::new(options, application_runtime, loaded, window, cx)
                    })
                },
            );
            if let Err(error) = result {
                eprintln!("无法打开 Orbit：{error:#}");
                cx.quit();
            } else if let (Ok((handle, view)), Some(report)) = (result, smoke_test) {
                let deadline_report = report.clone();
                let deadline_view = view.downgrade();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(90))
                        .await;
                    let diagnostic = cx.update(|cx| {
                        deadline_view
                            .upgrade()
                            .map(|view| view.read(cx).startup_diagnostic(cx))
                    });
                    let receipt = serde_json::json!({
                        "connected": false,
                        "diagnostic": diagnostic,
                    });
                    let _ = cx
                        .background_executor()
                        .spawn(async move { std::fs::write(deadline_report, receipt.to_string()) })
                        .await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
                let _ = handle.update(cx, |_, window, cx| {
                    view.update(cx, |view, cx| view.enable_control_probe(cx));
                    smoke_frame(
                        report,
                        0,
                        view.downgrade(),
                        window,
                        smoke_account_edit.then_some(accounts::AccountCheck::Loading),
                        false,
                    )
                });
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
    let mut shutdown_receipt = shutdown_report
        .as_ref()
        .and_then(|path| {
            std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        });
    if let (Some(path), Some(receipt)) = (&shutdown_report, &mut shutdown_receipt) {
        receipt["event_loop_exited"] = true.into();
        std::fs::write(path, serde_json::to_vec(receipt)?)?;
    }
    drop(writer);
    if let (Some(path), Some(receipt)) = (&shutdown_report, &mut shutdown_receipt) {
        receipt["storage_flushed"] = true.into();
        std::fs::write(path, serde_json::to_vec(receipt)?)?;
    }
    // Keep the executor alive until views release transports and the durable writer drains.
    drop(runtime);
    Ok(())
}

/// Observe actual platform frames; ordinary launches never install this bounded CI observer.
fn smoke_frame(
    report: std::path::PathBuf,
    frame: u8,
    view: WeakEntity<workbench::Workbench>,
    window: &Window,
    mut account_check: Option<accounts::AccountCheck>,
    account_edit_verified: bool,
) {
    window.on_next_frame(move |window, cx| {
        let configuration_required = view
            .upgrade()
            .is_some_and(|view| view.read(cx).configuration_required(cx));
        let ready = (configuration_required
            && view
                .upgrade()
                .is_some_and(|view| view.read(cx).backend_ready(cx)))
            || view
                .upgrade()
                .is_some_and(|view| view.read(cx).ready_for_input(cx));
        let details_probe =
            cfg!(feature = "test-support") && std::env::var_os("ORBIT_GUI_DETAILS_PROBE").is_some();
        if !ready && (!details_probe || frame == 0) {
            smoke_frame(
                report,
                0,
                view,
                window,
                account_check,
                account_edit_verified,
            );
            return;
        }
        #[cfg(feature = "test-support")]
        if details_probe {
            if matches!(frame, 1 | 4 | 6 | 8..=31)
                && let Some(directory) = std::env::var_os("ORBIT_GUI_CAPTURE_DIR")
            {
                let directory = std::path::PathBuf::from(directory);
                let _ = std::fs::create_dir_all(&directory);
                if cfg!(target_os = "linux")
                    && let Ok(display) = std::env::var("DISPLAY")
                {
                    let _ = std::process::Command::new("ffmpeg")
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
                            "1600x1000",
                            "-i",
                        ])
                        .arg(display)
                        .args(["-frames:v", "1"])
                        .arg(directory.join(format!("details-{frame}.png")))
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                }
            }
            match view.update(cx, |view, cx| view.check_details(frame, window, cx)) {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => {
                    if frame == 9 {
                        window.dispatch_event(
                            PlatformInput::KeyDown(KeyDownEvent {
                                keystroke: Keystroke::parse("escape").expect("static Escape key"),
                                is_held: false,
                                prefer_character_input: false,
                            }),
                            cx,
                        );
                    }
                    let handle = window.window_handle();
                    cx.spawn(async move |cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(300))
                            .await;
                        let _ = handle.update(cx, |_, window, _| {
                            smoke_frame(
                                report,
                                frame + 1,
                                view,
                                window,
                                account_check,
                                account_edit_verified,
                            );
                            window.refresh();
                        });
                    })
                    .detach();
                    window.refresh();
                    return;
                }
                error => {
                    eprintln!("Detail presentation acceptance failed: {error:?}");
                    cx.quit();
                    return;
                }
            }
        }
        if account_check.is_some() && frame == 0 {
            smoke_frame(
                report,
                1,
                view,
                window,
                account_check,
                account_edit_verified,
            );
            return;
        }
        if let Some(step) = &mut account_check {
            if matches!(step, accounts::AccountCheck::Loading)
                && let Some(view) = view.upgrade()
                && view
                    .read(cx)
                    .panel_visibility(cx)
                    .is_some()
                && let Err(error) = check_titlebar_controls(&view, window, cx)
            {
                eprintln!("侧栏按钮验收失败：{error:#}");
                cx.quit();
                return;
            }
            if let Some(directory) = std::env::var_os("ORBIT_GUI_CAPTURE_DIR") {
                let path =
                    std::path::PathBuf::from(directory).join(format!("{}.png", step.frame_name()));
                if cfg!(target_os = "linux") {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    // This branch is confined to the explicit isolated native acceptance probe.
                    if let Ok(display) = std::env::var("DISPLAY") {
                        let _ = std::process::Command::new("ffmpeg")
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
                            .arg(&path)
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status();
                    }
                }
            }
            let result = view.update(cx, |view, cx| view.check_account_edit(step, window, cx));
            match result {
                Ok(Ok(complete)) => {
                    smoke_frame(
                        report,
                        0,
                        view,
                        window,
                        if complete { None } else { account_check },
                        complete,
                    );
                }
                error => {
                    eprintln!("账号流程验收失败：{error:?}");
                    cx.quit();
                }
            }
            return;
        }
        if frame < 2 {
            smoke_frame(
                report,
                frame + 1,
                view,
                window,
                account_check,
                account_edit_verified,
            );
            return;
        }
        let bounds = window.bounds();
        let receipt = serde_json::json!({
            "frames": frame + 1,
            "width": f32::from(bounds.size.width),
            "height": f32::from(bounds.size.height),
            "platform": std::env::consts::OS,
            "connected": true,
            "configuration_required": configuration_required,
            "embedded_icons": assets::embedded_icon_count().unwrap_or_default(),
            "account_edit_verified": account_edit_verified,
            "details_verified": details_probe,
        });
        let task = cx
            .background_executor()
            .spawn(async move {
                std::fs::write(report, serde_json::to_vec(&receipt)?)?;
                Ok::<_, anyhow::Error>(())
            });
        cx.spawn(async move |cx| {
            if let Err(error) = task.await {
                eprintln!("窗口验收记录失败：{error:#}");
            }
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

/// Exercise real rendered controls in the explicitly requested isolated native probe.
fn check_titlebar_controls(
    view: &Entity<workbench::Workbench>,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    let initial = view
        .read(cx)
        .panel_visibility(cx)
        .ok_or_else(|| anyhow::anyhow!("conversation panels are unavailable"))?;
    let maximized = window.is_maximized();
    for (index, id) in ["toggle-sidebar", "toggle-inspector"]
        .into_iter()
        .enumerate()
    {
        let bounds = view
            .read(cx)
            .control_bounds(index)
            .ok_or_else(|| anyhow::anyhow!("missing rendered control {id}"))?;
        let position = bounds.center();
        for click_count in 1..=2 {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    ..Default::default()
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    position,
                    button: MouseButton::Left,
                    click_count,
                    ..Default::default()
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    position,
                    button: MouseButton::Left,
                    click_count,
                    ..Default::default()
                }),
                cx,
            );
            let expected = if click_count == 2 {
                initial
            } else if index == 0 {
                (!initial.0, initial.1)
            } else {
                (initial.0, !initial.1)
            };
            anyhow::ensure!(
                view.read(cx).panel_visibility(cx) == Some(expected),
                "{id} did not toggle its panel"
            );
            anyhow::ensure!(
                window.is_maximized() == maximized,
                "{id} double click changed window state"
            );
        }
    }
    eprintln!("PASS: both titlebar controls toggle on click and double click without maximizing");
    Ok(())
}
