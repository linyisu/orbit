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
            let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
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
                let _ = handle.update(cx, |_, window, _| {
                    smoke_frame(report, 0, view.downgrade(), window)
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
) {
    window.on_next_frame(move |window, cx| {
        let configuration_required = view
            .upgrade()
            .is_some_and(|view| view.read(cx).configuration_required(cx));
        let ready = configuration_required
            || view
                .upgrade()
                .is_some_and(|view| view.read(cx).ready_for_input(cx));
        if !ready {
            smoke_frame(report, 0, view, window);
            return;
        }
        if frame < 2 {
            smoke_frame(report, frame + 1, view, window);
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
