use std::{
    fmt::Write as _,
    num::NonZeroU16,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{
    ImageEncoder, RgbaImage,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};
use ratatui::{
    buffer::{Buffer, CellDiffOption, CellWidth},
    layout::{Rect, Size},
};

use super::{View, view::Geometry};

// Budget for RGBA pixels and the encoded PNG transfer together.
const MAX_PIXELS: u64 = super::MAX_CACHE_BYTES as u64 / 12;
const PAYLOAD_CHUNK_BYTES: usize = 3072;
const PLACEMENT_ID: u32 = 1;

pub(super) struct Image {
    pub width: u32,
    pub height: u32,
    pub cell: Size,
    id: u32,
    upload: String,
    transmitted: AtomicBool,
}

impl Image {
    pub fn new(pixels: &RgbaImage, cell: Size) -> Result<Self> {
        let (width, height) = pixels.dimensions();
        if u64::from(width) * u64::from(height) > MAX_PIXELS {
            bail!("图片超出终端直接预览的缓存预算");
        }
        let mut png = Vec::new();
        PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
            .write_image(
                pixels.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )?;
        let id = super::image_id();
        let mut upload = String::new();
        let chunks = png.chunks(PAYLOAD_CHUNK_BYTES);
        let count = chunks.len();
        for (index, bytes) in chunks.enumerate() {
            if index == 0 {
                write!(upload, "\x1b_Ga=t,f=100,t=d,i={id},q=2,")?;
            } else {
                upload.push_str("\x1b_Gq=2,");
            }
            write!(upload, "m={};", u8::from(index + 1 < count))?;
            STANDARD.encode_string(bytes, &mut upload);
            upload.push_str("\x1b\\");
        }
        Ok(Self {
            width,
            height,
            cell,
            id,
            upload,
            transmitted: AtomicBool::new(false),
        })
    }

    pub fn bytes(&self) -> usize {
        self.upload.len() + self.width as usize * self.height as usize * 4
    }

    pub fn geometry(&self, area: Rect, view: View) -> Geometry {
        view.geometry(
            self.width as f32,
            self.height as f32,
            u32::from(area.width) * u32::from(self.cell.width),
            u32::from(area.height) * u32::from(self.cell.height),
        )
    }

    fn remove(&self, commands: &mut String) {
        write!(
            commands,
            "\x1b_Ga=d,d=i,i={},p={PLACEMENT_ID},q=2\x1b\\",
            self.id
        )
        .unwrap();
        self.transmitted.store(false, Ordering::Release);
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        if let Ok(mut deletions) = super::DELETIONS.lock() {
            deletions.push(self.id);
        }
    }
}

#[derive(Clone)]
pub(super) struct Placement {
    pub image: Arc<Image>,
    pub body: Rect,
    pub view: View,
}

impl Placement {
    pub fn area(&self) -> Rect {
        let geometry = self.image.geometry(self.body, self.view);
        let width = geometry.width.div_ceil(u32::from(self.image.cell.width)) as u16;
        let height = geometry.height.div_ceil(u32::from(self.image.cell.height)) as u16;
        Rect::new(
            self.body.x + (self.body.width - width) / 2,
            self.body.y + (self.body.height - height) / 2,
            width,
            height,
        )
    }

    fn same_view(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.image, &other.image) && self.body == other.body && self.view == other.view
    }

    fn display(&self, commands: &mut String) {
        if !self.image.transmitted.swap(true, Ordering::AcqRel) {
            commands.push_str(&self.image.upload);
        }
        let area = self.area();
        let geometry = self.image.geometry(self.body, self.view);
        let (x, y, width, height) = geometry.bounds(self.image.width, self.image.height);
        // A stable placement id replaces the crop in the terminal without another upload.
        write!(commands, "\x1b7\x1b[{};{}H\x1b_Ga=p,i={},p={PLACEMENT_ID},q=2,C=1,x={x},y={y},w={width},h={height},c={},r={}\x1b\\\x1b8",
            area.y + 1, area.x + 1, self.image.id, area.width, area.height).unwrap();
    }
}

pub(super) fn paint(
    current: Option<&Placement>,
    previous: Option<&Placement>,
    buffer: &mut Buffer,
) {
    let mut commands = String::new();
    if let Some(previous) = previous
        && current.is_none_or(|next| !Arc::ptr_eq(&next.image, &previous.image))
    {
        previous.image.remove(&mut commands);
    }
    if let Some(current) = current
        && previous.is_none_or(|old| !current.same_view(old))
    {
        current.display(&mut commands);
    }
    if !commands.is_empty()
        && let Some(cell) = buffer.cell_mut((buffer.area.x, buffer.area.y))
    {
        let width = NonZeroU16::new(cell.cell_width().max(1)).unwrap();
        commands.push_str(cell.symbol());
        cell.set_symbol(&commands)
            .set_diff_option(CellDiffOption::ForcedWidth(width));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_data_is_uploaded_once_and_moves_resize_and_close_only_update_placements() {
        let pixels = RgbaImage::from_fn(128, 64, |x, y| image::Rgba([x as u8, y as u8, 100, 128]));
        let image = Arc::new(Image::new(&pixels, Size::new(2, 2)).unwrap());
        let mut encoded = String::new();
        for chunk in image.upload.split("\x1b_G").skip(1) {
            let (parameters, payload) = chunk.split_once(';').unwrap();
            assert!(parameters.contains("q=2"));
            let payload = payload.strip_suffix("\x1b\\").unwrap();
            assert!(payload.len() <= 4096);
            encoded.push_str(payload);
        }
        let decoded = image::load_from_memory(&STANDARD.decode(encoded).unwrap())
            .unwrap()
            .into_rgba8();
        assert_eq!(decoded.dimensions(), pixels.dimensions());
        assert_eq!(decoded.get_pixel(127, 63).0, [127, 63, 100, 128]);

        let mut placement = Placement {
            image,
            body: Rect::new(3, 4, 20, 10),
            view: View::default(),
        };
        let buffer = || {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 20));
            buffer[(0, 0)].set_symbol("中");
            buffer
        };
        let mut frame = buffer();
        paint(Some(&placement), None, &mut frame);
        assert!(frame[(0, 0)].symbol().contains("a=t,f=100"));
        assert!(frame[(0, 0)].symbol().ends_with("\x1b8中"));
        assert_eq!(frame[(0, 0)].cell_width(), 2);
        let mut frame = buffer();
        paint(Some(&placement), Some(&placement), &mut frame);
        assert_eq!(frame[(0, 0)].symbol(), "中");

        for (zoom, pan, body) in [
            (200, (-1000, -1000), placement.body),
            (400, (1000, 1000), placement.body),
            (200, (0, 0), Rect::new(3, 4, 30, 12)),
        ] {
            let previous = placement.clone();
            placement.view.zoom = zoom;
            placement.view.pan = pan;
            placement.body = body;
            let mut frame = buffer();
            paint(Some(&placement), Some(&previous), &mut frame);
            let wire = frame[(0, 0)].symbol();
            assert!(!wire.contains("a=t"));
            assert!(wire.contains("a=p") && wire.contains("p=1,q=2,C=1"));
            assert!(wire.len() < 256);
            assert_eq!(placement.area().intersection(body), placement.area());
        }
        let mut frame = buffer();
        paint(None, Some(&placement), &mut frame);
        assert!(frame[(0, 0)].symbol().contains("a=d,d=i"));
        assert!(!frame[(0, 0)].symbol().contains("d=I"));
        let mut frame = buffer();
        paint(Some(&placement), None, &mut frame);
        assert!(frame[(0, 0)].symbol().contains("a=t,f=100"));
    }
}
