//! Android activity integration; portable UI tests use explicit unavailable results.
use orbit_client::remote::Attachment;

#[cfg(target_os = "android")]
#[path = "platform/android.rs"]
mod android;
#[cfg(target_os = "android")]
pub use android::{camera_permission, device_name, pick_file, scan};

#[cfg(not(target_os = "android"))]
pub async fn pick_file() -> Result<Option<Attachment>, String> {
    Err("请在 Android 手机上选择附件".into())
}
#[cfg(not(target_os = "android"))]
pub async fn camera_permission() -> Result<(), String> {
    Err("请在 Android 手机上扫码".into())
}
#[cfg(not(target_os = "android"))]
pub fn scan(_: tokio::sync::mpsc::Sender<Result<Scan, String>>) {}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub enum Scan {
    Preview(Vec<u8>),
    Code(String),
}

/// Decode only Orbit invitations, ignoring unrelated QR codes.
#[cfg(any(target_os = "android", test))]
pub fn decode(image: image::GrayImage) -> Option<String> {
    let mut prepared = rqrr::PreparedImage::prepare(image);
    prepared
        .detect_grids()
        .into_iter()
        .find_map(|grid| {
            let (_, content) = grid.decode().ok()?;
            content
                .starts_with("orbit-pair:")
                .then_some(content)
        })
}

#[cfg(not(target_os = "android"))]
pub fn device_name() -> String {
    "Android 手机".into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_pairing_codes_decode_at_camera_resolution() {
        let code = format!("orbit-pair:1:{}", "sample-credential-data".repeat(65));
        let image = qrcode::QrCode::new(code.as_bytes())
            .unwrap()
            .render::<image::Luma<u8>>()
            .min_dimensions(640, 640)
            .build();
        assert_eq!(super::decode(image.clone()).as_deref(), Some(code.as_str()));
        assert_eq!(
            super::decode(image::imageops::rotate90(&image)).as_deref(),
            Some(code.as_str())
        );
        let unrelated = qrcode::QrCode::new(b"https://example.test")
            .unwrap()
            .render::<image::Luma<u8>>()
            .min_dimensions(320, 320)
            .build();
        assert!(super::decode(unrelated).is_none());
    }
}
