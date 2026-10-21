//! Desktop preview of the Android acceptance surface, without service startup.
fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(orbit_mobile_probe::open);
}
