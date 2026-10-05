use image::DynamicImage;

pub(super) const PAN_RANGE: i16 = 1000;
pub(super) const PAN_STEP: i16 = PAN_RANGE / 10;
const FIT_ZOOM: u16 = 100;
pub(super) const ZOOM_LEVELS: &[u16] = &[25, 50, 75, 100, 125, 150, 200, 300, 400, 600, 800];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct View {
    pub zoom: u16,
    pub pan: (i16, i16),
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: FIT_ZOOM,
            pan: (0, 0),
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
        }
    }
}

impl Geometry {
    pub fn crop(&self, image: DynamicImage) -> DynamicImage {
        let x = (self.x / self.scale).floor() as u32;
        let y = (self.y / self.scale).floor() as u32;
        let right = ((self.x + self.width as f32) / self.scale).ceil() as u32;
        let bottom = ((self.y + self.height as f32) / self.scale).ceil() as u32;
        image
            .crop_imm(
                x.min(image.width() - 1),
                y.min(image.height() - 1),
                right.min(image.width()).saturating_sub(x).max(1),
                bottom.min(image.height()).saturating_sub(y).max(1),
            )
            .resize_exact(
                self.width,
                self.height,
                image::imageops::FilterType::Lanczos3,
            )
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
        for zoom in [200, 400, 800] {
            for pan in [-PAN_RANGE, PAN_RANGE] {
                let geometry = View {
                    zoom,
                    pan: (pan, pan),
                }
                .geometry(100.0, 100.0, 20, 20);
                assert_eq!((geometry.width, geometry.height), (20, 20));
                assert!(geometry.overflow.0 > 0.0 && geometry.overflow.1 > 0.0);
                let cropped = geometry
                    .crop(DynamicImage::ImageRgba8(image.clone()))
                    .to_rgba8();
                let expected = if pan < 0 {
                    [255, 255, 0, 255]
                } else {
                    [0, 0, 0, 255]
                };
                assert_eq!(cropped.get_pixel(10, 10).0, expected);
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
}
