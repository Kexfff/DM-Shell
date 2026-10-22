//! Images registered by key (tray icons, avatars, album art): raw pixels, PNG, JPEG or
//! SVG files. Each is rasterized once per pixel size it's drawn at, so animation frames
//! only blit. Photos fill their square (cropped, not squashed) with rounded corners.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tiny_skia::{FilterQuality, IntSize, Pixmap, PixmapMut, PixmapPaint, Transform};

#[derive(Clone, Debug, PartialEq)]
pub enum ImageSource {
    /// Premultiplied RGBA.
    Rgba {
        width: u32,
        height: u32,
        data: Arc<Vec<u8>>,
    },
    File(PathBuf),
}

struct Entry {
    source: ImageSource,
    /// Icon-like images get dark monochrome artwork tinted light; photos are left alone.
    icon: bool,
    /// Rasterized at each pixel size it was drawn at; `None` if it failed to load.
    sized: HashMap<(u32, u32), Option<Pixmap>>,
}

#[derive(Default)]
pub struct Images {
    entries: HashMap<String, Entry>,
}

impl Images {
    /// Registers an image; `icon` marks artwork (tray, app icons) that may be tinted to
    /// stay visible on the black islands, as opposed to photos such as avatars.
    pub fn set(&mut self, key: &str, source: ImageSource, icon: bool) {
        if self
            .entries
            .get(key)
            .is_some_and(|e| e.source == source && e.icon == icon)
        {
            return;
        }
        self.entries.insert(
            key.to_owned(),
            Entry {
                source,
                icon,
                sized: HashMap::new(),
            },
        );
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.entries.retain(|k, _| keep(k));
    }

    /// Draws image `key` into `rect` (pixels: x, y, width, height).
    pub fn draw(
        &mut self,
        key: &str,
        rect: (i32, i32, u32, u32),
        alpha: f32,
        pixmap: &mut PixmapMut,
    ) {
        let (x, y, w, h) = rect;
        if w == 0 || h == 0 {
            return;
        }
        let Some(entry) = self.entries.get_mut(key) else {
            return;
        };
        let source = &entry.source;
        let icon = entry.icon;
        let image = entry.sized.entry((w, h)).or_insert_with(|| {
            let image = rasterize(source, w, h, !icon).map(|image| {
                if icon {
                    lighten_if_dark_monochrome(image)
                } else {
                    round_corners(image)
                }
            });
            if image.is_none() {
                log::debug!("could not load image {source:?}");
            }
            image
        });
        if let Some(image) = image {
            let paint = PixmapPaint {
                opacity: alpha,
                ..PixmapPaint::default()
            };
            pixmap.draw_pixmap(x, y, image.as_ref(), &paint, Transform::identity(), None);
        }
    }
}

/// `cover`: crop to the target's shape first (photos), instead of stretching.
fn rasterize(source: &ImageSource, w: u32, h: u32, cover: bool) -> Option<Pixmap> {
    let fit = |image: Pixmap| {
        let image = if cover { crop_to(&image, w, h) } else { image };
        scale(&image, w, h)
    };
    match source {
        ImageSource::Rgba {
            width,
            height,
            data,
        } => {
            let size = IntSize::from_wh(*width, *height)?;
            let image = Pixmap::from_vec(data.to_vec(), size)?;
            Some(fit(image))
        }
        ImageSource::File(path) => {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext.eq_ignore_ascii_case("svg") {
                let data = std::fs::read(path).ok()?;
                let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
                let size = tree.size();
                let mut out = Pixmap::new(w, h)?;
                let transform =
                    Transform::from_scale(w as f32 / size.width(), h as f32 / size.height());
                resvg::render(&tree, transform, &mut out.as_mut());
                Some(out)
            } else {
                let data = std::fs::read(path).ok()?;
                // By content, not name: cached album art has no reliable extension.
                let image = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
                    decode_jpeg(&data)?
                } else {
                    Pixmap::decode_png(&data).ok()?
                };
                Some(fit(image))
            }
        }
    }
}

/// Light text color the islands use; dark monochrome icons are tinted to it.
const LIGHT: [u8; 3] = [245, 245, 247];

/// Icon themes made for light panels draw tray icons dark gray, which vanishes on the
/// black islands. A mostly-gray, dark icon is tinted light (keeping its shape and
/// alpha); colored icons are left alone.
fn lighten_if_dark_monochrome(mut image: Pixmap) -> Pixmap {
    let (mut opaque, mut colored, mut luminance) = (0u32, 0u32, 0f32);
    for px in image.pixels() {
        let a = px.alpha();
        if a < 64 {
            continue;
        }
        let c = px.demultiply();
        let (r, g, b) = (c.red(), c.green(), c.blue());
        opaque += 1;
        if r.max(g).max(b) - r.min(g).min(b) > 48 {
            colored += 1;
        }
        luminance += (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
    }
    let monochrome = opaque > 0 && colored * 10 < opaque;
    if !monochrome || luminance / opaque as f32 >= 0.45 {
        return image;
    }
    for px in image.data_mut().as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        for (channel, light) in px.iter_mut().zip(LIGHT) {
            *channel = ((u32::from(light) * a + 127) / 255) as u8;
        }
    }
    image
}

fn decode_jpeg(data: &[u8]) -> Option<Pixmap> {
    use zune_jpeg::zune_core::{
        bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions,
    };
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), options);
    let pixels = decoder.decode().ok()?;
    let info = decoder.info()?;
    // Opaque, so already premultiplied.
    let size = IntSize::from_wh(u32::from(info.width), u32::from(info.height))?;
    Pixmap::from_vec(pixels, size)
}

/// The middle of `image` with the aspect ratio of `w`×`h`.
fn crop_to(image: &Pixmap, w: u32, h: u32) -> Pixmap {
    let (iw, ih) = (image.width() as f32, image.height() as f32);
    let target = w as f32 / h as f32;
    let (cw, ch) = if iw / ih > target {
        ((ih * target).round(), ih)
    } else {
        (iw, (iw / target).round())
    };
    let (cw, ch) = (cw.max(1.0) as u32, ch.max(1.0) as u32);
    if (cw, ch) == (image.width(), image.height()) {
        return image.clone();
    }
    let Some(mut out) = Pixmap::new(cw, ch) else {
        return image.clone();
    };
    let dx = -((image.width() - cw) as i32 / 2);
    let dy = -((image.height() - ch) as i32 / 2);
    out.draw_pixmap(
        dx,
        dy,
        image.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    out
}

/// Rounds a photo's corners (a fifth of its size), like the islands around it.
fn round_corners(mut image: Pixmap) -> Pixmap {
    let (w, h) = (image.width(), image.height());
    let r = w.min(h) as f32 * 0.2;
    if r < 1.0 {
        return image;
    }
    let pixels = image.pixels_mut();
    for y in 0..h {
        for x in 0..w {
            // Distance from the nearest corner circle's center, for pixels in a corner.
            let cx = (x as f32 + 0.5).clamp(r, w as f32 - r);
            let cy = (y as f32 + 0.5).clamp(r, h as f32 - r);
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let coverage = (r - d + 0.5).clamp(0.0, 1.0);
            if coverage < 1.0 {
                let p = pixels[(y * w + x) as usize];
                let scale = |c: u8| (c as f32 * coverage).round() as u8;
                pixels[(y * w + x) as usize] = tiny_skia::PremultipliedColorU8::from_rgba(
                    scale(p.red()),
                    scale(p.green()),
                    scale(p.blue()),
                    scale(p.alpha()),
                )
                .unwrap_or(p);
            }
        }
    }
    image
}

/// Resamples to `w`×`h`, halving first for big downscales so small icons stay smooth.
fn scale(image: &Pixmap, w: u32, h: u32) -> Pixmap {
    let mut current = image.clone();
    while current.width() >= w * 2 && current.height() >= h * 2 {
        current = resample(&current, current.width() / 2, current.height() / 2);
    }
    resample(&current, w, h)
}

fn resample(image: &Pixmap, w: u32, h: u32) -> Pixmap {
    let mut out = Pixmap::new(w.max(1), h.max(1)).expect("non-zero size");
    let transform = Transform::from_scale(
        w as f32 / image.width() as f32,
        h as f32 / image.height() as f32,
    );
    let paint = PixmapPaint {
        quality: FilterQuality::Bicubic,
        ..PixmapPaint::default()
    };
    out.draw_pixmap(0, 0, image.as_ref(), &paint, transform, None);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rgba: [u8; 4]) -> Pixmap {
        let data = rgba.repeat(8 * 8);
        Pixmap::from_vec(data, IntSize::from_wh(8, 8).unwrap()).unwrap()
    }

    #[test]
    fn dark_monochrome_icons_turn_light() {
        let dark = lighten_if_dark_monochrome(solid([40, 40, 40, 255]));
        assert_eq!(dark.pixel(0, 0).unwrap().red(), LIGHT[0]);

        // Premultiplied half-transparent dark gray keeps its alpha.
        let faded = lighten_if_dark_monochrome(solid([20, 20, 20, 128]));
        let px = faded.pixel(0, 0).unwrap();
        assert_eq!(px.alpha(), 128);
        assert!(px.demultiply().red() > 230);
    }

    #[test]
    fn colored_and_light_icons_are_untouched() {
        let blue = lighten_if_dark_monochrome(solid([30, 80, 200, 255]));
        assert_eq!(blue.pixel(0, 0).unwrap().blue(), 200);
        let light = lighten_if_dark_monochrome(solid([220, 220, 220, 255]));
        assert_eq!(light.pixel(0, 0).unwrap().red(), 220);
    }

    #[test]
    fn rgba_images_draw_scaled() {
        let mut images = Images::default();
        let red = [255u8, 0, 0, 255].repeat(64 * 64);
        images.set(
            "red",
            ImageSource::Rgba {
                width: 64,
                height: 64,
                data: Arc::new(red),
            },
            false,
        );
        let mut target = Pixmap::new(40, 40).unwrap();
        images.draw("red", (4, 4, 28, 28), 1.0, &mut target.as_mut());
        let center = target.pixel(18, 18).unwrap();
        assert_eq!((center.red(), center.alpha()), (255, 255));
        assert_eq!(target.pixel(1, 1).unwrap().alpha(), 0);
        images.draw("missing", (0, 0, 8, 8), 1.0, &mut target.as_mut());
    }
}
