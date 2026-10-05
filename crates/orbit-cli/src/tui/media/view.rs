use image::{DynamicImage, RgbaImage};

pub(super) const PAN_RANGE: i16 = 1000;
pub(super) const PAN_STEP: i16 = PAN_RANGE / 10;
const FIT_ZOOM: u16 = 100;
pub(super) const MIN_ZOOM: u16 = 25;
pub(super) const MAX_ZOOM: u16 = 800;
pub(super) const ZOOM_RATIO: f32 = 1.06;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum Quality {
    Interactive,
    #[default]
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct View {
    pub zoom: u16,
    pub pan: (i16, i16),
    pub quality: Quality,
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: FIT_ZOOM,
            pan: (0, 0),
            quality: Quality::Full,
        }
    }
}

pub(super) struct Geometry {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
    pub width: u32,
    pub height: u32,
    pub overflow: (f32, f32),
    quality: Quality,
}

impl View {
    /// Crop before encoding so zoom never allocates an enlarged full image.
    pub fn geometry(self, width: f32, height: f32, max_w: u32, max_h: u32) -> Geometry {
        let scale = (max_w as f32 / width).min(max_h as f32 / height) * f32::from(self.zoom)
            / f32::from(FIT_ZOOM);
        let overflow = (
            (width * scale - max_w as f32).max(0.0),
            (height * scale - max_h as f32).max(0.0),
        );
        let position = |pan: i16, excess: f32| {
            excess * f32::from(pan.clamp(-PAN_RANGE, PAN_RANGE) + PAN_RANGE)
                / f32::from(2 * PAN_RANGE)
        };
        Geometry {
            scale,
            x: position(self.pan.0, overflow.0),
            y: position(self.pan.1, overflow.1),
            width: (width * scale).ceil().max(1.0).min(max_w as f32) as u32,
            height: (height * scale).ceil().max(1.0).min(max_h as f32) as u32,
            overflow,
            quality: self.quality,
        }
    }
}

impl Geometry {
    pub fn crop(&self, image: &DynamicImage) -> DynamicImage {
        let bounds = self.bounds(image.width(), image.height());
        if self.quality == Quality::Interactive
            && let Some(pixels) = image.as_rgba8()
        {
            return DynamicImage::ImageRgba8(self.sample(pixels, bounds));
        }
        image
            .crop_imm(bounds.0, bounds.1, bounds.2, bounds.3)
            .resize_exact(
                self.width,
                self.height,
                match self.quality {
                    Quality::Interactive => image::imageops::FilterType::Nearest,
                    Quality::Full => image::imageops::FilterType::Lanczos3,
                },
            )
    }

    pub(super) fn bounds(&self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let x = ((self.x / self.scale).floor() as u32).min(width - 1);
        let y = ((self.y / self.scale).floor() as u32).min(height - 1);
        let right = ((self.x + self.width as f32) / self.scale).ceil() as u32;
        let bottom = ((self.y + self.height as f32) / self.scale).ceil() as u32;
        (
            x,
            y,
            right.min(width).saturating_sub(x).max(1),
            bottom.min(height).saturating_sub(y).max(1),
        )
    }

    /// Point sampling avoids a full floating-point resampling intermediate.
    fn sample(&self, source: &RgbaImage, bounds: (u32, u32, u32, u32)) -> RgbaImage {
        let (x, y, width, height) = bounds;
        let columns: Vec<_> = (0..self.width)
            .map(|column| {
                (x + (((column as f32 + 0.5) * width as f32 / self.width as f32) as u32)
                    .min(width - 1)) as usize
                    * 4
            })
            .collect();
        let mut output = RgbaImage::new(self.width, self.height);
        let stride = source.width() as usize * 4;
        for (index, row) in output.chunks_exact_mut(self.width as usize * 4).enumerate() {
            let source_y = (y
                + (((index as f32 + 0.5) * height as f32 / self.height as f32) as u32)
                    .min(height - 1)) as usize;
            let source_row = &source.as_raw()[source_y * stride..(source_y + 1) * stride];
            for (pixel, x) in row.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
                pixel.copy_from_slice(&source_row[*x..*x + 4]);
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_crops_both_axes_and_reaches_image_edges_without_growing_the_canvas() {
        let image = image::RgbaImage::from_fn(100, 100, |x, y| {
            image::Rgba([
                if x < 50 { 255 } else { 0 },
                if y < 50 { 255 } else { 0 },
                0,
                255,
            ])
        });
        for quality in [Quality::Full, Quality::Interactive] {
            for zoom in [200, 400, 800] {
                for pan in [-PAN_RANGE, PAN_RANGE] {
                    let geometry = View {
                        zoom,
                        pan: (pan, pan),
                        quality,
                    }
                    .geometry(100.0, 100.0, 20, 20);
                    assert_eq!((geometry.width, geometry.height), (20, 20));
                    assert!(geometry.overflow.0 > 0.0 && geometry.overflow.1 > 0.0);
                    let cropped = geometry
                        .crop(&DynamicImage::ImageRgba8(image.clone()))
                        .to_rgba8();
                    let expected = if pan < 0 {
                        [255, 255, 0, 255]
                    } else {
                        [0, 0, 0, 255]
                    };
                    assert_eq!(cropped.get_pixel(10, 10).0, expected);
                }
            }
        }
        let fit = View::default().geometry(100.0, 100.0, 60, 20);
        assert_eq!((fit.width, fit.height), (20, 20));
        assert_eq!(fit.overflow, (0.0, 0.0));
        let small = View {
            zoom: 25,
            ..Default::default()
        }
        .geometry(100.0, 100.0, 20, 20);
        assert_eq!((small.width, small.height), (5, 5));
    }

    #[test]
    fn interactive_sampling_preserves_alpha_and_reaches_the_last_source_pixel() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(8, 8, |x, y| {
            image::Rgba([x as u8, y as u8, 100, 128])
        }));
        let geometry = View {
            zoom: 200,
            pan: (PAN_RANGE, PAN_RANGE),
            quality: Quality::Interactive,
        }
        .geometry(8.0, 8.0, 4, 4);
        let cropped = geometry.crop(&image).into_rgba8();
        assert_eq!(cropped.dimensions(), (4, 4));
        assert_eq!(cropped.get_pixel(0, 0).0, [4, 4, 100, 128]);
        assert_eq!(cropped.get_pixel(3, 3).0, [7, 7, 100, 128]);

        let fractional = View {
            zoom: 400,
            pan: (333, 333),
            quality: Quality::Interactive,
        }
        .geometry(8.0, 8.0, 4, 4);
        let cropped = fractional.crop(&image).into_rgba8();
        assert_eq!(cropped.get_pixel(0, 0).0, [3, 3, 100, 128]);
        assert_eq!(cropped.get_pixel(3, 3).0, [5, 5, 100, 128]);
    }
}
