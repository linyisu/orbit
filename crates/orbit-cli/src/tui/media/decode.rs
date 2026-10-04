use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context as _, Result, bail};
use image::{DynamicImage, ImageReader, RgbaImage};
use ratatui::layout::Size;
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::kitty::Kitty,
    sliced::SlicedProtocol,
};
use ratex_layout::LayoutOptions;
use ratex_types::{color::Color, math_style::MathStyle};
use resvg::{tiny_skia, usvg};

use super::{Key, Prepared, Source};

const MAX_BYTES: usize = 20 * 1024 * 1024;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Decoder {
    fonts: Option<Arc<usvg::fontdb::Database>>,
    remote_bytes: HashMap<String, Arc<Vec<u8>>>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Decoder {
    pub fn prepare(&mut self, key: &Key, picker: &Picker) -> Result<Prepared> {
        let cell = picker.font_size();
        let max_w = (u32::from(key.width) * u32::from(cell.width)).clamp(1, 2048);
        let max_h = (u32::from(key.height) * u32::from(cell.height)).clamp(1, 2048);
        let mut baseline = None;
        let image = match &key.source {
            Source::Image(source) => {
                let bytes = if super::is_remote(source) {
                    if let Some(bytes) = self.remote_bytes.get(source) {
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
                                .block_on(orbit_tools::web_fetch::fetch_image(source))?,
                        );
                        while self.remote_bytes.values().map(|b| b.len()).sum::<usize>()
                            + bytes.len()
                            > 2 * MAX_BYTES
                        {
                            if let Some(key) = self.remote_bytes.keys().next().cloned() {
                                self.remote_bytes.remove(&key);
                            } else {
                                break;
                            }
                        }
                        self.remote_bytes.insert(source.clone(), bytes.clone());
                        bytes
                    }
                } else {
                    let path = local_path(source, &key.workspace)?;
                    let mut bytes = Vec::new();
                    std::fs::File::open(&path)?
                        .take(MAX_BYTES as u64 + 1)
                        .read_to_end(&mut bytes)?;
                    Arc::new(bytes)
                };
                if bytes.len() > MAX_BYTES {
                    bail!("图片超过 20 MiB");
                }
                let svg = image::guess_format(&bytes).is_err();
                if svg {
                    if self.fonts.is_none() {
                        let mut db = usvg::fontdb::Database::new();
                        db.load_system_fonts();
                        self.fonts = Some(Arc::new(db));
                    }
                    raster_svg(&bytes, max_w, max_h, self.fonts.clone(), key.large)?
                } else {
                    let mut reader =
                        ImageReader::new(Cursor::new(bytes.as_slice())).with_guessed_format()?;
                    let mut limits = image::Limits::default();
                    limits.max_image_width = Some(8192);
                    limits.max_image_height = Some(8192);
                    limits.max_alloc = Some(MAX_PIXELS * 4);
                    reader.limits(limits);
                    let image = reader.decode()?;
                    if u64::from(image.width()) * u64::from(image.height()) > MAX_PIXELS {
                        bail!("图片像素尺寸过大");
                    }
                    image.resize(max_w, max_h, image::imageops::FilterType::Triangle)
                }
            }
            Source::Math { tex, display } => {
                if tex.len() > 8192 {
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
                let tree = usvg::Tree::from_str(&svg, &usvg::Options::default())?;
                let image = raster_svg(
                    svg.as_bytes(),
                    max_w,
                    max_h.saturating_sub(u32::from(cell.height)).max(1),
                    None,
                    false,
                )?;
                let ratio = image.height() as f64 / f64::from(tree.size().height());
                let baseline_px = baseline_px * ratio;
                let cell_h = u32::from(cell.height);
                let offset = ((f64::from(cell_h) * 0.75
                    - baseline_px.rem_euclid(f64::from(cell_h)))
                .rem_euclid(f64::from(cell_h)))
                .round() as u32;
                baseline =
                    Some(((baseline_px + f64::from(offset)) / f64::from(cell_h)).floor() as u16);
                let mut padded = RgbaImage::new(image.width(), image.height() + offset);
                image::imageops::overlay(&mut padded, &image.to_rgba8(), 0, i64::from(offset));
                DynamicImage::ImageRgba8(padded)
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
        let kitty_id = (picker.protocol_type() == ProtocolType::Kitty).then(super::image_id);
        let protocol = if let Some(id) = kitty_id {
            let mut padded = RgbaImage::new(
                u32::from(size.width) * u32::from(cell.width),
                u32::from(size.height) * u32::from(cell.height),
            );
            image::imageops::overlay(&mut padded, &image.to_rgba8(), 0, 0);
            SlicedProtocol::Kitty(Kitty::new(
                DynamicImage::ImageRgba8(padded),
                size,
                id,
                picker.tmux_detected(),
                false,
            )?)
        } else {
            SlicedProtocol::new(picker, image, Some(size))?
        };
        Ok(Prepared {
            size: protocol.size(),
            protocol,
            source: key.source.clone(),
            baseline: baseline
                .unwrap_or(size.height / 2)
                .min(size.height.saturating_sub(1)),
            kitty_id,
        })
    }
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
    let mut options = usvg::Options::default();
    if let Some(fonts) = fonts {
        options.fontdb = fonts;
    }
    // A preview may not read arbitrary external files referenced by an SVG.
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    let resolver = usvg::ImageHrefResolver::default_data_resolver();
    options.image_href_resolver.resolve_data = Box::new(move |mime, data, opts| {
        if data.len() > MAX_BYTES {
            return None;
        }
        // Embedded SVGs recurse through the same resolver. Raster images must
        // also obey the source pixel limit before resvg allocates their bitmap.
        if let Ok(reader) = ImageReader::new(Cursor::new(data.as_slice())).with_guessed_format()
            && reader.format().is_some()
        {
            let (w, h) = reader.into_dimensions().ok()?;
            if u64::from(w) * u64::from(h) > MAX_PIXELS {
                return None;
            }
        }
        resolver(mime, data, opts)
    });
    let tree = usvg::Tree::from_data(bytes, &options).context("无法解析 SVG")?;
    let size = tree.size();
    let scale = (max_w as f32 / size.width()).min(max_h as f32 / size.height());
    let scale = if upscale { scale } else { scale.min(1.0) };
    let width = (size.width() * scale).ceil().max(1.0) as u32;
    let height = (size.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).context("SVG 尺寸过大")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia is premultiplied; image/Kitty require straight RGBA.
    let bytes = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let p = p.demultiply();
            [p.red(), p.green(), p.blue(), p.alpha()]
        })
        .collect();
    Ok(DynamicImage::ImageRgba8(
        RgbaImage::from_raw(width, height, bytes).context("SVG 像素转换失败")?,
    ))
}
