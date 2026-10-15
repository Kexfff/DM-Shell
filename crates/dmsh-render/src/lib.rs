//! Rasterizes a [`Scene`] with tiny-skia. Used by the Wayland surface and by headless
//! PNG output, so both always draw exactly the same pixels.

use dmsh_core::{Color, RoundRect, Scene};
pub use tiny_skia;
use tiny_skia::{FillRule, Paint, PathBuilder, PixmapMut, Transform};

/// Clears `pixmap` and draws `scene`, scaling logical coordinates by `scale`.
pub fn render(scene: &Scene, scale: f32, pixmap: &mut PixmapMut) {
    pixmap.fill(tiny_skia::Color::TRANSPARENT);
    let transform = Transform::from_scale(scale, scale);
    for shape in &scene.shapes {
        draw_round_rect(shape, transform, pixmap);
    }
}

fn draw_round_rect(shape: &RoundRect, transform: Transform, pixmap: &mut PixmapMut) {
    let r = shape.rect;
    let Some(path) = rounded_rect(r.x, r.y, r.w, r.h, shape.radius) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(to_skia(shape.color));
    paint.anti_alias = true;
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

fn to_skia(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a)
}

/// Draws `scene` into a `wl_shm` ARGB8888 buffer of `width`×`height` pixels.
/// Returns `false` if the buffer doesn't match the dimensions.
pub fn render_argb8888(scene: &Scene, scale: f32, buf: &mut [u8], width: u32, height: u32) -> bool {
    let Some(mut pixmap) = PixmapMut::from_bytes(buf, width, height) else {
        return false;
    };
    render(scene, scale, &mut pixmap);
    rgba_to_argb8888(buf);
    true
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
    use dmsh_core::activity::RightView;
    use dmsh_core::{ActivityView, Layout, Priority, Theme, View};
    use tiny_skia::Pixmap;

    #[test]
    fn island_centers_are_opaque_and_outside_is_transparent() {
        let scale = 1.75;
        let theme = Theme::default();
        let newest = ActivityView {
            id: "a".into(),
            title: "a".into(),
            body: None,
            progress: None,
            priority: Priority::Low,
        };
        let view = View {
            center: None,
            right: Some(RightView { newest, more: 0 }),
            clock: None,
        };
        let layout = Layout::compute(1645.0, &theme, &view);
        let mut pixmap = Pixmap::new(2879, 560).unwrap();
        render(&layout.scene(&theme, &view), scale, &mut pixmap.as_mut());

        for r in [layout.center, layout.right.unwrap()] {
            let cx = ((r.x + r.w / 2.0) * scale) as u32;
            let cy = ((r.y + r.h / 2.0) * scale) as u32;
            assert_eq!(pixmap.pixel(cx, cy).unwrap().alpha(), 255);
        }
        assert_eq!(pixmap.pixel(5, 100).unwrap().alpha(), 0);
    }

    #[test]
    fn argb_conversion_swaps_red_and_blue() {
        let mut px = [1, 2, 3, 4];
        rgba_to_argb8888(&mut px);
        assert_eq!(px, [3, 2, 1, 4]);
    }
}
