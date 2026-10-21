//! Isolated Android platform acceptance surface; never connects to a workspace.
mod view;

use gpui_kit::*;

/// Open the isolated acceptance surface after the platform has supplied a window.
pub fn open(cx: &mut App) {
    gpui_kit::init(cx);
    cx.open_window(WindowOptions::default(), |window, cx| {
        let view = cx.new(|cx| view::Probe::new(window, cx));
        cx.new(|cx| gpui_kit::component::Root::new(view, window, cx))
    })
    .expect("open mobile acceptance window");
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("orbit-mobile-probe"),
    );
    gpui_mobile::android::jni::install_panic_hook();
    let _platform = gpui_mobile::android::jni::init_platform(&app);
    let Some(platform) = gpui_mobile::android::jni::shared_platform() else {
        log::error!("Android platform initialization failed");
        return;
    };
    Application::with_platform(platform.into_rc())
        .with_assets(gpui_kit::assets::Assets)
        .run(open);
}
