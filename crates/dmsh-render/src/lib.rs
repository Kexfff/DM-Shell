//! Rasterizes a [`Scene`] with tiny-skia and cosmic-text. Used by the Wayland surface and
//! by headless PNG output, so both always draw exactly the same pixels.

mod icons;
mod images;
mod text;

use dmsh_core::{Color, Draw, Measure, Rect, RoundRect, Scene, TextStyle};
pub use icons::names as icon_names;
pub use images::ImageSource;
pub use tiny_skia;
use tiny_skia::{FillRule, Paint, PathBuilder, PixmapMut, Transform};

use crate::text::{Clip, TextEngine};

/// Owns fonts, glyph and icon caches. One per process; also measures text for layout.
pub struct Renderer {
    text: TextEngine,
    icons: icons::Icons,
    images: images::Images,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Renderer {
    /// `font` is the preferred UI font family (falls back to Inter, then sans-serif).
    pub fn new(font: Option<&str>) -> Self {
        Self {
            text: TextEngine::new(font),
            icons: icons::Icons::default(),
            images: images::Images::default(),
        }
    }

    /// Registers (or replaces) an image that scenes can draw by `key`.
    /// Registers (or replaces) an image that scenes can draw by `key`. `icon` marks
    /// artwork that may be tinted light to show on the black islands (not photos).
    pub fn set_image(&mut self, key: &str, source: ImageSource, icon: bool) {
        self.images.set(key, source, icon);
    }

    pub fn has_image(&self, key: &str) -> bool {
        self.images.contains(key)
    }

    /// Forgets images whose keys aren't kept.
    pub fn retain_images(&mut self, keep: impl Fn(&str) -> bool) {
        self.images.retain(keep);
    }

    /// Clears `pixmap` and draws `scene`, scaling logical coordinates by `scale`.
    pub fn render(&mut self, scene: &Scene, scale: f32, pixmap: &mut PixmapMut) {
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        for group in &scene.groups {
            let alpha = group.alpha.clamp(0.0, 1.0);
            for item in &group.items {
                match item {
                    Draw::Rect(r) => {
                        let r = match group.clip {
                            Some(clip) => match intersect(r.rect, clip) {
                                Some(rect) => RoundRect { rect, ..*r },
                                None => continue,
                            },
                            None => *r,
                        };
                        fill_round_rect(&r, alpha, scale, pixmap);
                    }
                    Draw::Text(t) => {
                        let clip = group.clip.map(|c| pixel_clip(c, scale));
                        self.text.draw(t, scale, alpha, clip, pixmap);
                    }
                    // Icons and images are small; draw them only once they fit the island.
                    Draw::Icon(i) => {
                        if fits(i.rect, group.clip) {
                            self.icons.draw(i, scale, alpha, pixmap);
                        }
                    }
                    Draw::Image(i) => {
                        if fits(i.rect, group.clip) {
                            let x = (i.rect.x * scale).round() as i32;
                            let y = (i.rect.y * scale).round() as i32;
                            let w = (i.rect.w * scale).round() as u32;
                            let h = (i.rect.h * scale).round() as u32;
                            self.images.draw(&i.key, (x, y, w, h), alpha, pixmap);
                        }
                    }
                }
            }
        }
    }

    /// Draws `scene` into a `wl_shm` ARGB8888 buffer of `width`×`height` pixels.
    /// Returns `false` if the buffer doesn't match the dimensions.
    pub fn render_argb8888(
        &mut self,
        scene: &Scene,
        scale: f32,
        buf: &mut [u8],
        width: u32,
        height: u32,
    ) -> bool {
        let Some(mut pixmap) = PixmapMut::from_bytes(buf, width, height) else {
            return false;
        };
        self.render(scene, scale, &mut pixmap);
        rgba_to_argb8888(buf);
        true
    }
}

impl Measure for Renderer {
    fn measure(
        &mut self,
        text: &str,
        style: TextStyle,
        max_width: f32,
        max_lines: usize,
    ) -> (f32, f32) {
        self.text.measure(text, style, max_width, max_lines)
    }
}

fn fits(rect: Rect, clip: Option<Rect>) -> bool {
    clip.is_none_or(|c| {
        intersect(rect, c).is_some_and(|r| r.w >= rect.w - 0.5 && r.h >= rect.h - 0.5)
    })
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

fn pixel_clip(r: Rect, scale: f32) -> Clip {
    Clip {
        x0: (r.x * scale).floor() as i32,
        y0: (r.y * scale).floor() as i32,
        x1: (r.right() * scale).ceil() as i32,
        y1: (r.bottom() * scale).ceil() as i32,
    }
}

fn fill_round_rect(shape: &RoundRect, alpha: f32, scale: f32, pixmap: &mut PixmapMut) {
    let r = shape.rect;
    let Some(path) = rounded_rect(r.x, r.y, r.w, r.h, shape.radius) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(to_skia(shape.color, alpha));
    paint.anti_alias = true;
    let transform = Transform::from_scale(scale, scale);
    pixmap.fill_path(&path, &paint, FillRule::Winding, transform, None);
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<tiny_skia::Path> {
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    // Circular arcs approximated by cubics; k is the standard quarter-circle constant.
    let k = r * 0.552_284_8;
    let (x1, y1) = (x + w, y + h);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x1 - r, y);
    pb.cubic_to(x1 - r + k, y, x1, y + r - k, x1, y + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x + r, y1);
    pb.cubic_to(x + r - k, y1, x, y1 - r + k, x, y1 - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn to_skia(c: Color, alpha: f32) -> tiny_skia::Color {
    let a = (c.a as f32 * alpha).round().clamp(0.0, 255.0) as u8;
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, a)
}

/// Converts tiny-skia's premultiplied RGBA bytes to `wl_shm` ARGB8888 (BGRA in memory on
/// little-endian) in place.
pub fn rgba_to_argb8888(bytes: &mut [u8]) {
    for px in bytes.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmsh_core::{Group, Theme, Weight};
    use tiny_skia::Pixmap;

    fn pill_scene(rect: Rect) -> Scene {
        Scene {
            groups: vec![Group {
                clip: None,
                alpha: 1.0,
                items: vec![Draw::Rect(RoundRect::pill(
                    rect,
                    Theme::default().pill_color,
                ))],
            }],
            islands: vec![rect],
        }
    }

    #[test]
    fn pill_is_opaque_inside_and_transparent_outside() {
        let scale = 1.75;
        let rect = Rect::new(100.0, 3.0, 200.0, 30.0);
        let mut pixmap = Pixmap::new(800, 70).unwrap();
        let mut renderer = Renderer::default();
        renderer.render(&pill_scene(rect), scale, &mut pixmap.as_mut());
        let cx = ((rect.x + rect.w / 2.0) * scale) as u32;
        let cy = ((rect.y + rect.h / 2.0) * scale) as u32;
        assert_eq!(pixmap.pixel(cx, cy).unwrap().alpha(), 255);
        assert_eq!(pixmap.pixel(5, 5).unwrap().alpha(), 0);
    }

    #[test]
    fn measures_text_and_ellipsizes() {
        let mut r = Renderer::default();
        let style = TextStyle::new(13.0, Weight::Semibold);
        let (short, h) = r.measure("Hi", style, 300.0, 1);
        let long_text = "wide ".repeat(100);
        let (long, _) = r.measure(&long_text, style, 300.0, 1);
        assert!(short > 5.0 && short < 40.0, "short = {short}");
        assert!(long <= 300.0, "ellipsized to fit: {long}");
        assert_eq!(h, style.line_height());
        let (_, wrapped) = r.measure(&long_text, style, 300.0, 3);
        assert_eq!(wrapped, 3.0 * style.line_height());
    }

    #[test]
    fn argb_conversion_swaps_red_and_blue() {
        let mut px = [1, 2, 3, 4];
        rgba_to_argb8888(&mut px);
        assert_eq!(px, [3, 2, 1, 4]);
    }
}
