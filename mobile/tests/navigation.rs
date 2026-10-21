use nsh_core::{
    geometry::{px, size},
    ui::Root,
};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

#[test]
fn home_navigation_and_scanner_failures_are_recoverable() {
    let mut config = EngineConfig::new(Root::of::<orbit_mobile::App>());
    config.viewport = size(px(360.0), px(640.0));
    config.ui_sets = vec![nsh_base::registry()];
    let mut app = Headless::start(config).unwrap();
    app.wait_for("text:连接你的电脑")
        .unwrap();
    assert!(!app.shows("有什么想做的？"));
    assert!(!app.shows("设备授权资料"));
    assert!(!app.shows("开源许可"));
    app.press("label:设置").unwrap();
    app.wait_for("text:连接与安全").unwrap();
    app.press("text:开源许可").unwrap();
    app.back().unwrap();
    app.back().unwrap();
    app.wait_for("text:连接你的电脑")
        .unwrap();
    app.press("text:扫描配对二维码")
        .unwrap();
    app.wait_for("text:请在 Android 手机上扫码")
        .unwrap();
    app.wait_for("text:连接你的电脑")
        .unwrap();
}
