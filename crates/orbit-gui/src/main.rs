mod accounts;
mod appearance;
mod assets;
mod backend;
mod conversation;
mod execution;
mod keymap;
mod math;
mod preferences;
mod theme;
mod usage;
mod workbench;
mod workspace;

use clap::Parser;
use gpui_kit::component::Theme;
use gpui_kit::*;
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--internal-ssh-askpass") {
        return orbit_client::interaction::askpass();
    }
    orbit_client::register_server_bundle(include_bytes!(env!("ORBIT_EMBEDDED_SERVER")))?;
    let mut options = backend::Options::parse();
    options.resolve_paths()?;
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let preferences = preferences::Preferences::load(options.preferences.as_deref(), &runtime)?;
    let (loaded, _writer) = workbench::storage::Storage::load(&options)?;
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
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
                    ..gpui_kit::component::TitleBar::window_options()
                },
                cx,
                |window, cx| {
                    window.set_window_title("Orbit");
                    cx.new(|cx| workbench::Workbench::new(options, runtime, loaded, window, cx))
                },
            );
            if let Err(error) = result {
                eprintln!("无法打开 Orbit：{error:#}");
                cx.quit();
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
    Ok(())
}
