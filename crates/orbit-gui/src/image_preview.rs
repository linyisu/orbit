//! One image viewer for message images and composer drafts.
use gpui_kit::component::{WindowExt, h_flex};
use gpui_kit::*;
use std::{path::PathBuf, sync::Arc};

struct ImagePreview {
    image: Result<Option<Arc<Image>>, SharedString>,
    dimensions: Option<(u32, u32)>,
    _load: Task<()>,
}

pub(crate) fn open(
    image: Result<Option<Arc<Image>>, SharedString>,
    dimensions: Option<(u32, u32)>,
    window: &mut Window,
    cx: &mut App,
) {
    let preview = cx.new(|_| ImagePreview {
        image,
        dimensions,
        _load: Task::ready(()),
    });
    show(preview, window, cx);
}

pub(crate) fn open_path(path: PathBuf, window: &mut Window, cx: &mut App) {
    let preview = cx.new(|cx: &mut Context<ImagePreview>| {
        let load = cx.background_spawn(async move { load_image(path) });
        ImagePreview {
            image: Ok(None),
            dimensions: None,
            _load: cx.spawn_in(window, async move |owner, cx| {
                let result = load.await;
                let _ = owner.update_in(cx, |preview, window, cx| {
                    match result {
                        Ok((image, dimensions)) => {
                            preview.image = Ok(Some(image));
                            preview.dimensions = Some(dimensions);
                        }
                        Err(error) => preview.image = Err(error),
                    }
                    cx.notify();
                    // The dialog's width depends on the loaded raster dimensions.
                    window.refresh();
                });
            }),
        }
    });
    show(preview, window, cx);
}

fn show(preview: Entity<ImagePreview>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let (width, _) = preview.read(cx).geometry(window);
        dialog.title("图片预览").width(width).child(preview.clone())
    });
}

fn load_image(path: PathBuf) -> Result<(Arc<Image>, (u32, u32)), SharedString> {
    let bytes = std::fs::read(path).map_err(|error| SharedString::from(error.to_string()))?;
    let format = image::guess_format(&bytes)
        .map_err(|error| SharedString::from(format!("无法识别图片：{error}")))?;
    let dimensions = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|error| SharedString::from(format!("无法读取图片：{error}")))?;
    let format = match format {
        image::ImageFormat::Png => ImageFormat::Png,
        image::ImageFormat::Jpeg => ImageFormat::Jpeg,
        image::ImageFormat::Gif => ImageFormat::Gif,
        image::ImageFormat::WebP => ImageFormat::Webp,
        _ => return Err("不支持的图片格式".into()),
    };
    Ok((Arc::new(Image::from_bytes(format, bytes)), dimensions))
}

impl ImagePreview {
    fn geometry(&self, window: &Window) -> (Pixels, Size<Pixels>) {
        let (width, height) = self.dimensions.unwrap_or((800, 600));
        let rem = f32::from(window.rem_size());
        let available = window.viewport_size();
        let (dialog_width, image_width, image_height) = fit(
            width,
            height,
            f32::from(available.width),
            f32::from(available.height),
            rem,
        );
        // Image dimensions are raster pixels; dialog insets follow the UI scale.
        (px(dialog_width), size(px(image_width), px(image_height)))
    }
}

fn fit(
    width: u32,
    height: u32,
    viewport_width: f32,
    viewport_height: f32,
    rem: f32,
) -> (f32, f32, f32) {
    let max_width = (viewport_width - rem * 4.).min(rem * 64.);
    let max_height = (viewport_height - rem * 10.).max(rem * 8.);
    let scale = ((max_width - rem * 2.5) / width.max(1) as f32)
        .min(max_height / height.max(1) as f32)
        .min(1.);
    let image_width = width as f32 * scale;
    let image_height = height as f32 * scale;
    (
        (image_width + rem * 2.5).max(rem * 20.).min(max_width),
        image_width,
        image_height,
    )
}

impl Render for ImagePreview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (_, fitted) = self.geometry(window);
        h_flex().justify_center().child(match &self.image {
            Ok(Some(image)) => img(image.clone())
                .w(fitted.width)
                .h(fitted.height)
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            Ok(None) => div().child("加载图片…").into_any_element(),
            Err(error) => div().child(error.clone()).into_any_element(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{fit, load_image};

    #[test]
    fn wide_and_tall_images_fit_the_same_viewer_at_normal_and_large_ui_scales() {
        for rem in [14., 20.] {
            for (viewport_width, viewport_height) in [(800., 560.), (1180., 800.)] {
                for (width, height) in [(2195, 470), (470, 2195), (80, 60)] {
                    let (dialog_width, image_width, image_height) =
                        fit(width, height, viewport_width, viewport_height, rem);
                    assert!(dialog_width <= viewport_width - rem * 4.);
                    assert!(image_width + rem * 2.5 <= dialog_width + 0.01);
                    assert!(image_height <= (viewport_height - rem * 10.).max(rem * 8.));
                    assert!(image_width <= width as f32);
                    assert!(image_height <= height as f32);
                    assert!(
                        (image_width / image_height - width as f32 / height as f32).abs() < 0.01
                    );
                }
            }
        }
    }

    #[test]
    fn unreadable_draft_image_returns_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.png");
        assert!(load_image(missing).is_err());
        let invalid = directory.path().join("invalid.png");
        std::fs::write(&invalid, b"not an image").unwrap();
        assert!(load_image(invalid).is_err());
    }

    #[test]
    fn local_draft_preview_uses_actual_raster_dimensions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wide.png");
        image::RgbaImage::new(320, 80).save(&path).unwrap();
        let (_, dimensions) = load_image(path).unwrap();
        assert_eq!(dimensions, (320, 80));
    }
}
