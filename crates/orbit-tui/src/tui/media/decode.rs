use std::{collections::HashMap, io::Cursor, path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result, bail};
use image::{DynamicImage, GenericImage, ImageReader, RgbaImage};
use ratatui::layout::Size;
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::kitty::Kitty,
    sliced::SlicedProtocol,
};
use ratex_layout::LayoutOptions;
use ratex_types::{color::Color, math_style::MathStyle};
use resvg::{tiny_skia, usvg};

use super::{Key, Network, Prepared, Source, View, view::Background};

const MAX_SOURCE_BYTES: usize = orbit_model::types::Attachment::MAX_IMAGE_BYTES;
const MAX_REMOTE_CACHE_BYTES: usize = 2 * MAX_SOURCE_BYTES;
const MAX_RENDER_EDGE_PIXELS: u32 = 2_048;
const MAX_PREVIEW_PIXELS: u64 = super::MAX_CACHE_BYTES as u64 / 8;
pub(super) const MAX_DECODE_EDGE_PIXELS: u32 = 8_192;
const MAX_TEX_BYTES: usize = 8_192;
pub(super) const MAX_SOURCE_PIXELS: u64 = 16 * 1_024 * 1_024;

#[derive(Default)]
pub(super) struct Decoder {
    fonts: Option<Arc<usvg::fontdb::Database>>,
    remote_bytes: HashMap<(Network, String), Arc<Vec<u8>>>,
    workspace_bytes: HashMap<(crate::workspace::Files, String), Arc<Vec<u8>>>,
    runtime: Option<tokio::runtime::Runtime>,
    assets: super::assets::Assets,
}

impl Decoder {
    pub fn prepare(&mut self, key: &Key, picker: &Picker) -> Result<Prepared> {
        let cell = picker.font_size();
        let viewport_w = (u32::from(key.width) * u32::from(cell.width)).max(1);
        let viewport_h = (u32::from(key.height) * u32::from(cell.height)).max(1);
        let (max_w, max_h) = if key.view.is_some() {
            (viewport_w, viewport_h)
        } else {
            (
                viewport_w.min(MAX_RENDER_EDGE_PIXELS),
                viewport_h.min(MAX_RENDER_EDGE_PIXELS),
            )
        };
        let mut baseline = None;
        let mut pan_extent = (0.0, 0.0);
        let image = match &key.source {
            Source::Image(source) => {
                let bytes = if super::is_remote(source) {
                    let network = key
                        .network
                        .as_ref()
                        .context("图片请求缺少网络配置")?;
                    let remote_key = (network.clone(), source.clone());
                    if let Some(bytes) = self.remote_bytes.get(&remote_key) {
                        bytes.clone()
                    } else {
                        if self.runtime.is_none() {
                            self.runtime = Some(
                                tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()?,
                            );
                        }

                        let bytes = Arc::new(
                            self.runtime
                                .as_ref()
                                .unwrap()
                                .block_on(network.0.fetch_image(source))?,
                        );

                        while self
                            .remote_bytes
                            .values()
                            .map(|b| b.len())
                            .sum::<usize>()
                            + bytes.len()
                            > MAX_REMOTE_CACHE_BYTES
                        {
                            if let Some(key) = self.remote_bytes.keys().next().cloned() {
                                self.remote_bytes.remove(&key);
                            } else {
                                break;
                            }
                        }

                        self.remote_bytes
                            .insert(remote_key, bytes.clone());
                        bytes
                    }
                } else if let Some(files) = &key.files {
                    if self.runtime.is_none() {
                        self.runtime = Some(
                            tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()?,
                        );
                    }
                    let path = local_path(source, &key.workspace)?;
                    let asset = (files.clone(), path.to_string_lossy().into_owned());
                    if let Some(bytes) = self.workspace_bytes.get(&asset) {
                        bytes.clone()
                    } else {
                        let bytes = Arc::new(
                            self.runtime
                                .as_ref()
                                .unwrap()
                                .block_on(files.read(&asset.1))?,
                        );
                        while self
                            .workspace_bytes
                            .values()
                            .map(|bytes| bytes.len())
                            .sum::<usize>()
                            + bytes.len()
                            > MAX_REMOTE_CACHE_BYTES
                        {
                            let Some(oldest) = self
                                .workspace_bytes
                                .keys()
                                .next()
                                .cloned()
                            else {
                                break;
                            };
                            self.workspace_bytes.remove(&oldest);
                        }
                        self.workspace_bytes
                            .insert(asset, bytes.clone());
                        bytes
                    }
                } else {
                    let path = local_path(source, &key.workspace)?;
                    self.assets.bytes(&path)?
                };
                if bytes.len() > MAX_SOURCE_BYTES {
                    bail!("图片超过 20 MiB");
                }
                let svg = image::guess_format(&bytes).is_err();
                if key.native {
                    if svg {
                        bail!("矢量图片使用媒体重绘预览");
                    }
                    let mut pixels = self.assets.raster(&bytes)?.to_rgba8();
                    composite_background(&mut pixels, key.view.unwrap_or_default().background);
                    let native = Arc::new(super::kitty_preview::Image::new(
                        &pixels,
                        Size::new(cell.width, cell.height),
                    )?);
                    return Ok(Prepared {
                        protocol: SlicedProtocol::Sliced(Vec::new()),
                        selected_protocol: None,
                        source: key.source.clone(),
                        size: Size::default(),
                        baseline: 0,
                        pan_extent: (0.0, 0.0),
                        kitty_id: None,
                        selected_kitty_id: None,
                        native: Some(native),
                    });
                }
                if svg {
                    if self.fonts.is_none() {
                        let mut db = usvg::fontdb::Database::new();
                        db.load_system_fonts();
                        self.fonts = Some(Arc::new(db));
                    }

                    let tree = self
                        .assets
                        .svg(&bytes, self.fonts.clone())?;
                    let (image, overflow) = raster_tree(&tree, max_w, max_h, key.large, key.view)?;
                    pan_extent = overflow;
                    image
                } else {
                    let image = self.assets.raster(&bytes)?;
                    if let Some(view) = key.view {
                        let geometry = view.geometry(
                            image.width() as f32,
                            image.height() as f32,
                            max_w,
                            max_h,
                        );
                        check_preview_size(geometry.width, geometry.height)?;
                        pan_extent = geometry.overflow;
                        geometry.crop(&image)
                    } else {
                        image.resize(max_w, max_h, image::imageops::FilterType::Triangle)
                    }
                }
            }

            Source::Math { tex, display } => {
                if tex.len() > MAX_TEX_BYTES {
                    bail!("公式超过 8192 字节");
                }
                let color = picker
                    .capabilities()
                    .iter()
                    .find_map(|c| match c {
                        ratatui_image::picker::Capability::Background(r, g, b)
                            if (u32::from(*r) * 3 + u32::from(*g) * 6 + u32::from(*b)) / 10
                                > 160 =>
                        {
                            Some([35, 40, 48])
                        }

                        _ => None,
                    })
                    .unwrap_or([220, 224, 232]);
                let (svg, baseline_px) = math_svg(
                    tex,
                    *display,
                    u32::from(cell.height).max(8) * if key.large { 2 } else { 1 },
                    color,
                )?;
                if let Some(view) = key.view {
                    let (image, overflow) =
                        raster_svg_view(svg.as_bytes(), max_w, max_h, None, true, Some(view))?;
                    pan_extent = overflow;
                    image
                } else {
                    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default())?;
                    let image = raster_svg(
                        svg.as_bytes(),
                        max_w,
                        max_h
                            .saturating_sub(u32::from(cell.height))
                            .max(1),
                        None,
                        false,
                    )?;
                    let ratio = image.height() as f64 / f64::from(tree.size().height());
                    let image = trim_math_sides(image);
                    // Text can advance only by whole cells. Fit the cropped ink
                    // to the nearest cell boundary instead of adding a blank
                    // strip on the right; leave height and baseline unchanged.
                    let cell_w = u32::from(cell.width);
                    let width = ((image.width() + cell_w / 2) / cell_w).max(1) * cell_w;
                    let image = image.resize_exact(
                        width.min(max_w),
                        image.height(),
                        image::imageops::FilterType::Lanczos3,
                    );
                    let baseline_px = baseline_px * ratio;
                    let cell_h = u32::from(cell.height);
                    let offset = ((f64::from(cell_h) * 0.75
                        - baseline_px.rem_euclid(f64::from(cell_h)))
                    .rem_euclid(f64::from(cell_h)))
                    .round() as u32;
                    baseline = Some(
                        ((baseline_px + f64::from(offset)) / f64::from(cell_h)).floor() as u16,
                    );
                    let mut padded = RgbaImage::new(image.width(), image.height() + offset);
                    image::imageops::overlay(&mut padded, &image.to_rgba8(), 0, i64::from(offset));
                    DynamicImage::ImageRgba8(padded)
                }
            }
        };
        let size = Size::new(
            image
                .width()
                .div_ceil(u32::from(cell.width))
                .min(u32::from(key.width)) as u16,
            image
                .height()
                .div_ceil(u32::from(cell.height))
                .min(u32::from(key.height)) as u16,
        );
        if key.view.is_some() {
            check_preview_size(
                u32::from(size.width) * u32::from(cell.width),
                u32::from(size.height) * u32::from(cell.height),
            )?;
        }
        // Inline selections need a pre-encoded variant; previews have no
        // selection and keep only the primary protocol within the cache budget.
        let mut padded = RgbaImage::new(
            u32::from(size.width) * u32::from(cell.width),
            u32::from(size.height) * u32::from(cell.height),
        );
        padded.copy_from(&image.into_rgba8(), 0, 0)?;
        // Math remains transparent, including baseline/cell padding. Ordinary
        // images get a matte only in display pixels, never in the source cache.
        if matches!(key.source, Source::Image(_)) {
            composite_background(&mut padded, key.view.unwrap_or_default().background);
        }
        let (selected_protocol, selected_kitty_id) = if key.view.is_none() {
            let (protocol, id) = encode(picker, selected_image(&padded), size)?;
            (Some(protocol), id)
        } else {
            (None, None)
        };
        let (protocol, kitty_id) = encode(picker, padded, size)?;
        Ok(Prepared {
            size: protocol.size(),
            protocol,
            selected_protocol,
            source: key.source.clone(),
            pan_extent: (
                pan_extent.0 / f32::from(cell.width),
                pan_extent.1 / f32::from(cell.height),
            ),
            baseline: baseline
                .unwrap_or(size.height / 2)
                .min(size.height.saturating_sub(1)),
            kitty_id,
            selected_kitty_id,
            native: None,
        })
    }
}

fn composite_background(pixels: &mut RgbaImage, background: Background) {
    let Some(color) = background.color() else {
        return;
    };
    for pixel in pixels.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        if alpha == 255 {
            continue;
        }
        for channel in 0..3 {
            pixel[channel] = ((u32::from(pixel[channel]) * alpha
                + u32::from(color[channel]) * (255 - alpha)
                + 127)
                / 255) as u8;
        }
        pixel[3] = 255;
    }
}

// Font advance widths can exceed the painted glyph, particularly for italic
// variables. Keep vertical metrics (and the baseline) but reserve only the
// painted horizontal extent plus one transparent pixel on either side.
fn trim_math_sides(image: DynamicImage) -> DynamicImage {
    let pixels = image.to_rgba8();
    let mut left = pixels.width();
    let mut right = 0;
    for (x, _, pixel) in pixels.enumerate_pixels() {
        if pixel[3] != 0 {
            left = left.min(x);
            right = right.max(x + 1);
        }
    }
    if left >= right {
        return image;
    }
    let left = left.saturating_sub(1);
    let right = (right + 1).min(pixels.width());
    DynamicImage::ImageRgba8(
        image::imageops::crop_imm(&pixels, left, 0, right - left, pixels.height()).to_image(),
    )
}

#[cfg(test)]
mod math_bounds_tests {
    use super::*;

    #[test]
    fn horizontal_crop_preserves_every_painted_pixel_and_vertical_metrics() {
        for tex in [
            "S",
            "R",
            "WE = 1",
            r"\overline{Q}",
            r"\frac{a}{b}",
            r"\int_0^1 x^2 dx",
        ] {
            let (svg, _) = math_svg(tex, false, 32, [220, 224, 232]).unwrap();
            let original = raster_svg(svg.as_bytes(), 1_200, 400, None, false).unwrap();
            let painted = |image: &DynamicImage| {
                image
                    .to_rgba8()
                    .pixels()
                    .filter(|pixel| pixel[3] != 0)
                    .count()
            };
            let trimmed = trim_math_sides(original.clone());
            assert_eq!(trimmed.height(), original.height());
            assert!(trimmed.width() <= original.width());
            assert_eq!(painted(&trimmed), painted(&original));
            let pixels = trimmed.to_rgba8();
            let first = pixels
                .enumerate_pixels()
                .filter(|(_, _, pixel)| pixel[3] != 0)
                .map(|(x, _, _)| x)
                .min()
                .unwrap();
            let last = pixels
                .enumerate_pixels()
                .filter(|(_, _, pixel)| pixel[3] != 0)
                .map(|(x, _, _)| x)
                .max()
                .unwrap();
            assert!(first <= 1 && pixels.width() - last <= 2);
        }
    }
}

fn check_preview_size(width: u32, height: u32) -> Result<()> {
    if u64::from(width) * u64::from(height) > MAX_PREVIEW_PIXELS {
        bail!("预览区域像素尺寸过大，请缩小窗口或降低缩放倍率");
    }
    Ok(())
}

fn encode(picker: &Picker, image: RgbaImage, size: Size) -> Result<(SlicedProtocol, Option<u32>)> {
    let image = DynamicImage::ImageRgba8(image);
    if picker.protocol_type() == ProtocolType::Kitty {
        let id = super::image_id();
        Ok((
            SlicedProtocol::Kitty(Kitty::new(
                image,
                size,
                id,
                picker.tmux_detected(),
                super::KITTY_TRANSMISSION_COMPRESSION,
            )?),
            Some(id),
        ))
    } else {
        Ok((SlicedProtocol::new(picker, image, Some(size))?, None))
    }
}

fn selected_image(image: &RgbaImage) -> RgbaImage {
    let ratatui::style::Color::Rgb(r, g, b) = crate::tui::theme::THEME.selection else {
        unreachable!("selection theme must use RGB")
    };
    let mut selected = image.clone();
    for pixel in selected.pixels_mut() {
        // Fill transparent TeX/SVG pixels; tint opaque photos so their selected
        // state is visible even when they cover the terminal cell background.
        let alpha = u32::from(pixel[3]);
        let ink = alpha * 3;
        for (channel, background) in pixel.0[..3].iter_mut().zip([r, g, b]) {
            *channel =
                ((u32::from(*channel) * ink + u32::from(background) * (1_020 - ink)) / 1_020) as u8;
        }

        pixel[3] = 255;
    }

    selected
}

fn local_path(source: &str, workspace: &std::path::Path) -> Result<PathBuf> {
    if let Ok(url) = url::Url::parse(source) {
        if url.scheme() == "file" {
            return url
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("无效的文件地址"));
        }

        bail!("不支持的图片地址协议");
    }

    let path = PathBuf::from(source);
    Ok(if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    })
}

pub(super) fn math_svg(
    tex: &str,
    display: bool,
    cell_height: u32,
    color: [u8; 3],
) -> Result<(String, f64)> {
    let ast = ratex_parser::parse(tex).map_err(|e| anyhow::anyhow!("{e}"))?;
    let options = LayoutOptions::default()
        .with_style(if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        })
        .with_color(Color {
            r: f32::from(color[0]) / 255.0,
            g: f32::from(color[1]) / 255.0,
            b: f32::from(color[2]) / 255.0,
            a: 1.0,
        });
    let tree = ratex_layout::layout(&ast, &options);
    let list = ratex_layout::to_display_list(&tree);
    if !list.width.is_finite() || !list.height.is_finite() || !list.depth.is_finite() {
        bail!("无效的公式尺寸");
    }
    let svg = ratex_svg::render_to_svg_with_color_syntax(
        &list,
        &ratex_svg::SvgOptions {
            font_size: f64::from(cell_height) * 9.0 / 16.0,
            padding: 0.5,
            embed_glyphs: true,
            ..Default::default()
        },
        ratex_svg::SvgColorSyntax::Rgb,
    );
    if svg.contains("<text") {
        bail!("公式字体不可用");
    }
    Ok((
        svg,
        (list.height * f64::from(cell_height) * 9.0 / 16.0 + 0.5) * 4.0 / 3.0,
    ))
}

pub(super) fn raster_svg(
    bytes: &[u8],
    max_w: u32,
    max_h: u32,
    fonts: Option<Arc<usvg::fontdb::Database>>,
    upscale: bool,
) -> Result<DynamicImage> {
    raster_svg_view(bytes, max_w, max_h, fonts, upscale, None).map(|(image, _)| image)
}

fn raster_svg_view(
    bytes: &[u8],
    max_w: u32,
    max_h: u32,
    fonts: Option<Arc<usvg::fontdb::Database>>,
    upscale: bool,
    view: Option<View>,
) -> Result<(DynamicImage, (f32, f32))> {
    let tree = parse_svg(bytes, fonts)?;
    raster_tree(&tree, max_w, max_h, upscale, view)
}

pub(super) fn parse_svg(
    bytes: &[u8],
    fonts: Option<Arc<usvg::fontdb::Database>>,
) -> Result<usvg::Tree> {
    let mut options = usvg::Options::default();
    if let Some(fonts) = fonts {
        options.fontdb = fonts;
    }
    // A preview may not read arbitrary external files referenced by an SVG.
    options
        .image_href_resolver
        .resolve_string = Box::new(|_, _| None);
    let resolver = usvg::ImageHrefResolver::default_data_resolver();
    options.image_href_resolver.resolve_data = Box::new(move |mime, data, opts| {
        if data.len() > MAX_SOURCE_BYTES {
            return None;
        }
        // Embedded SVGs recurse through the same resolver. Raster images must
        // also obey the source pixel limit before resvg allocates their bitmap.
        if let Ok(reader) = ImageReader::new(Cursor::new(data.as_slice())).with_guessed_format()
            && reader.format().is_some()
        {
            let (w, h) = reader.into_dimensions().ok()?;
            if u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
                return None;
            }
        }

        resolver(mime, data, opts)
    });
    usvg::Tree::from_data(bytes, &options).context("无法解析 SVG")
}

fn raster_tree(
    tree: &usvg::Tree,
    max_w: u32,
    max_h: u32,
    upscale: bool,
    view: Option<View>,
) -> Result<(DynamicImage, (f32, f32))> {
    let size = tree.size();
    let (width, height, transform, overflow) = if let Some(view) = view {
        let geometry = view.geometry(size.width(), size.height(), max_w, max_h);
        check_preview_size(geometry.width, geometry.height)?;
        (
            geometry.width,
            geometry.height,
            tiny_skia::Transform::from_scale(geometry.scale, geometry.scale)
                .post_translate(-geometry.x, -geometry.y),
            geometry.overflow,
        )
    } else {
        let scale = (max_w as f32 / size.width()).min(max_h as f32 / size.height());
        let scale = if upscale { scale } else { scale.min(1.0) };
        (
            (size.width() * scale).ceil().max(1.0) as u32,
            (size.height() * scale).ceil().max(1.0) as u32,
            tiny_skia::Transform::from_scale(scale, scale),
            (0.0, 0.0),
        )
    };
    let mut pixmap = tiny_skia::Pixmap::new(width, height).context("SVG 尺寸过大")?;
    resvg::render(tree, transform, &mut pixmap.as_mut());
    // tiny-skia is premultiplied; image/Kitty require straight RGBA.
    let bytes = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let p = p.demultiply();
            [p.red(), p.green(), p.blue(), p.alpha()]
        })
        .collect();
    Ok((
        DynamicImage::ImageRgba8(
            RgbaImage::from_raw(width, height, bytes).context("SVG 像素转换失败")?,
        ),
        overflow,
    ))
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn svg_view_renders_a_bounded_crop_at_the_requested_pan_and_preserves_alpha() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="50" height="100" fill="red"/><rect x="50" width="50" height="100" fill="blue" fill-opacity="0.5"/></svg>"#;
        for (pan, expected) in [(-1_000, [255, 0, 0, 255]), (1_000, [0, 0, 255, 128])] {
            let view = View {
                zoom: 800,
                pan: (pan, 0),
                ..Default::default()
            };
            let (image, overflow) = raster_svg_view(svg, 40, 20, None, true, Some(view)).unwrap();
            assert_eq!((image.width(), image.height()), (40, 20));
            assert!(overflow.0 > 0.0 && overflow.1 > 0.0);
            assert_eq!(image.to_rgba8().get_pixel(20, 10).0, expected);
        }
    }

    #[test]
    fn selection_fills_transparency_and_tints_opaque_pixels_without_changing_geometry() {
        let mut image = RgbaImage::new(3, 1);
        image.put_pixel(0, 0, image::Rgba([255, 255, 255, 0]));
        image.put_pixel(1, 0, image::Rgba([230, 100, 50, 255]));
        image.put_pixel(2, 0, image::Rgba([220, 224, 232, 128]));
        let selected = selected_image(&image);
        let ratatui::style::Color::Rgb(r, g, b) = crate::tui::theme::THEME.selection else {
            unreachable!()
        };
        assert_eq!(selected.dimensions(), image.dimensions());
        assert_eq!(selected.get_pixel(0, 0).0, [r, g, b, 255]);
        assert_ne!(selected.get_pixel(1, 0), image.get_pixel(1, 0));
        assert!(
            selected
                .pixels()
                .all(|pixel| pixel[3] == 255)
        );
        // The original remains transparent.
        assert_eq!(image.get_pixel(0, 0)[3], 0);
    }
}
