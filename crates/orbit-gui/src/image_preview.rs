//! One image viewer for message images and composer drafts.
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
};
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
                    // Refit the image when its dimensions become available.
                    window.refresh();
                });
            }),
        }
    });
    show(preview, window, cx);
}

pub(crate) fn init(cx: &mut App) {
    gpui_kit::base::Root::register_plugin::<PreviewHost>(cx, |_, cx| PreviewHost {
        preview: None,
        focus: cx.focus_handle(),
        previous_focus: None,
    });
}

struct PreviewHost {
    preview: Option<Entity<ImagePreview>>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
}
impl gpui_kit::base::RootPlugin for PreviewHost {}
impl PreviewHost {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = None;
        if let Some(focus) = self.previous_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }
}
impl Render for PreviewHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(preview) = self.preview.clone() else {
            return div().into_any_element();
        };
        gpui_kit::base::Dialog::new(cx)
            .focus_handle(self.focus.clone())
            .on_ok(|_, _, _| false)
            .on_close(cx.listener(|view, _, window, cx| view.close(window, cx)))
            .popup(preview)
            .into_any_element()
    }
}
fn show(preview: Entity<ImagePreview>, window: &mut Window, cx: &mut App) {
    if let Some(host) = gpui_kit::base::Root::read(window, cx).plugin::<PreviewHost>() {
        host.update(cx, |host, cx| {
            if host.preview.is_none() {
                host.previous_focus = window.focused(cx);
            }
            host.preview = Some(preview);
            host.focus.focus(window, cx);
            cx.notify();
        });
    }
}
fn close(window: &mut Window, cx: &mut App) {
    if let Some(host) = gpui_kit::base::Root::read(window, cx).plugin::<PreviewHost>() {
        host.update(cx, |host, cx| host.close(window, cx));
    }
}

fn load_image(path: PathBuf) -> Result<(Arc<Image>, (u32, u32)), SharedString> {
    let bytes = orbit_model::types::Attachment::read_image_bytes(&path)
        .map_err(|error| SharedString::from(error.to_string()))?;
    decode_image(bytes)
}

pub(crate) fn decode_image(bytes: Vec<u8>) -> Result<(Arc<Image>, (u32, u32)), SharedString> {
    if bytes.len() > orbit_model::types::Attachment::MAX_IMAGE_BYTES {
        return Err("图片超过 20 MiB".into());
    }
    let Ok(format) = image::guess_format(&bytes) else {
        return decode_svg(&bytes);
    };
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

fn decode_svg(bytes: &[u8]) -> Result<(Arc<Image>, (u32, u32)), SharedString> {
    // Document images cannot read sibling files or absolute hrefs on the desktop.
    let mut options = resvg::usvg::Options::default();
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    static FONTS: std::sync::LazyLock<Arc<resvg::usvg::fontdb::Database>> =
        std::sync::LazyLock::new(|| {
            let mut fonts = resvg::usvg::fontdb::Database::new();
            fonts.load_system_fonts();
            Arc::new(fonts)
        });
    options.fontdb = FONTS.clone();
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|error| SharedString::from(format!("无法识别图片：{error}")))?;
    let size = tree.size();
    let dimensions = (size.width().ceil() as u32, size.height().ceil() as u32);
    // SVG dimensions are source pixels; bound raster work independently of UI zoom.
    let scale = (4096. / size.width().max(size.height())).min(1.);
    let mut pixels = resvg::tiny_skia::Pixmap::new(
        (size.width() * scale).ceil().max(1.) as u32,
        (size.height() * scale).ceil().max(1.) as u32,
    )
    .ok_or_else(|| SharedString::from("图片尺寸无效"))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    let png = pixels
        .encode_png()
        .map_err(|error| SharedString::from(error.to_string()))?;
    Ok((
        Arc::new(Image::from_bytes(ImageFormat::Png, png)),
        dimensions,
    ))
}

impl ImagePreview {
    fn geometry(&self, window: &Window) -> (Pixels, Size<Pixels>) {
        let (width, height) = self.dimensions.unwrap_or((800, 600));
        let rem = f32::from(window.rem_size());
        let available = window.viewport_size();
        let (stage_width, image_width, image_height) = fit(
            width,
            height,
            f32::from(available.width),
            f32::from(available.height),
            rem,
        );
        // Image dimensions are raster pixels; viewer margins follow the UI scale.
        (px(stage_width), size(px(image_width), px(image_height)))
    }
}

fn fit(
    width: u32,
    height: u32,
    viewport_width: f32,
    viewport_height: f32,
    rem: f32,
) -> (f32, f32, f32) {
    let max_width = (viewport_width - rem * 6.).max(1.);
    let max_height = (viewport_height - rem * 8.).max(1.);
    // Enlarge small attachments as well as fitting large screenshots. A bounded
    // enlargement keeps tiny raster assets from filling the entire window.
    let scale = (max_width / width.max(1) as f32)
        .min(max_height / height.max(1) as f32)
        .min(4.);
    (
        max_width,
        width.max(1) as f32 * scale,
        height.max(1) as f32 * scale,
    )
}

impl Render for ImagePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (_, fitted) = self.geometry(window);
        div()
            .id("image-viewer")
            .size_full()
            .relative()
            .occlude()
            .bg(cx.theme().background.opacity(0.96))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                cx.stop_propagation();
                close(window, cx);
            })
            .child(match &self.image {
                Ok(Some(image)) => div()
                    .w(fitted.width)
                    .h(fitted.height)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        img(image.clone())
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element(),
                Ok(None) => div().child("加载图片…").into_any_element(),
                Err(error) => div().child(error.clone()).into_any_element(),
            })
            .child(
                div()
                    .absolute()
                    .top_4()
                    .right_4()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("close-image-preview")
                            .ghost()
                            .small()
                            .icon(gpui_kit::assets::IconName::X)
                            .accessibility_label("关闭图片预览")
                            .tooltip("关闭图片预览（Esc）")
                            .on_click(|_, window, cx| close(window, cx)),
                    ),
            )
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
                    let (stage_width, image_width, image_height) =
                        fit(width, height, viewport_width, viewport_height, rem);
                    assert!(stage_width <= viewport_width - rem * 6.);
                    assert!(image_width <= stage_width + 0.01);
                    assert!(image_height <= (viewport_height - rem * 8.).max(1.));
                    assert!(image_width <= width as f32 * 4.);
                    assert!(image_height <= height as f32 * 4.);
                    if width == 80 {
                        assert!(image_width > width as f32);
                    }
                    assert!(
                        (image_width / image_height - width as f32 / height as f32).abs() < 0.01
                    );
                }
            }
        }
    }

    #[test]
    fn vector_images_are_rasterized_and_cannot_load_desktop_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private.png");
        image::RgbaImage::from_pixel(12, 8, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        let source = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="8"><image href="{}" width="12" height="8"/></svg>"#,
            path.display()
        );
        let (image, dimensions) = super::decode_image(source.into_bytes()).unwrap();
        assert_eq!(dimensions, (12, 8));
        let raster = image::load_from_memory(image.bytes()).unwrap().into_rgba8();
        assert!(raster.pixels().all(|pixel| pixel[3] == 0));
        let (image, dimensions) = super::decode_image(br##"<svg xmlns="http://www.w3.org/2000/svg" width="8192" height="2048"><rect width="8192" height="2048" fill="#123456"/></svg>"##.to_vec()).unwrap();
        assert_eq!(dimensions, (8192, 2048));
        let raster = image::load_from_memory(image.bytes()).unwrap().into_rgba8();
        assert_eq!(raster.dimensions(), (4096, 1024));
        assert_eq!(raster.get_pixel(0, 0).0, [0x12, 0x34, 0x56, 255]);
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
