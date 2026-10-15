//! Images registered by key (tray icons): raw pixels, PNG or SVG files. Each is
//! rasterized once per pixel size it's drawn at, so animation frames only blit.

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
    /// Rasterized at each pixel size it was drawn at; `None` if it failed to load.
    sized: HashMap<(u32, u32), Option<Pixmap>>,
}

#[derive(Default)]
pub struct Images {
    entries: HashMap<String, Entry>,
}

impl Images {
    pub fn set(&mut self, key: &str, source: ImageSource) {
        if self.entries.get(key).is_some_and(|e| e.source == source) {
            return;
        }
        self.entries.insert(
            key.to_owned(),
            Entry {
                source,
                sized: HashMap::new(),
            },
        );
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
        let image = entry.sized.entry((w, h)).or_insert_with(|| {
            let image = rasterize(source, w, h);
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

fn rasterize(source: &ImageSource, w: u32, h: u32) -> Option<Pixmap> {
    match source {
        ImageSource::Rgba {
            width,
            height,
            data,
        } => {
            let size = IntSize::from_wh(*width, *height)?;
            let image = Pixmap::from_vec(data.to_vec(), size)?;
            Some(scale(&image, w, h))
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
                Some(scale(&Pixmap::load_png(path).ok()?, w, h))
            }
        }
    }
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
        );
        let mut target = Pixmap::new(40, 40).unwrap();
        images.draw("red", (4, 4, 28, 28), 1.0, &mut target.as_mut());
        let center = target.pixel(18, 18).unwrap();
        assert_eq!((center.red(), center.alpha()), (255, 255));
        assert_eq!(target.pixel(1, 1).unwrap().alpha(), 0);
        images.draw("missing", (0, 0, 8, 8), 1.0, &mut target.as_mut());
    }
}
