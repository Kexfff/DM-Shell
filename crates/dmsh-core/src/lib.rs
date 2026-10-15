//! Pure shell logic: geometry, theme and layout. No I/O, no Wayland.
//!
//! All coordinates here are logical pixels; renderers apply the output scale.

/// Axis-aligned rectangle in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Smallest integer rectangle covering this one, as `(x, y, w, h)`.
    pub fn round_out(&self) -> (i32, i32, i32, i32) {
        let x0 = self.x.floor() as i32;
        let y0 = self.y.floor() as i32;
        let x1 = (self.x + self.w).ceil() as i32;
        let y1 = (self.y + self.h).ceil() as i32;
        (x0, y0, x1 - x0, y1 - y0)
    }
}

/// Non-premultiplied RGBA color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color::rgba(0, 0, 0, 255);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// Visual constants. The default is the Apple-like solid black pill theme.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Height reserved at the top of the output (layer-shell exclusive zone).
    pub strip_height: u32,
    /// Total surface height; expanded islands float over windows inside it.
    pub surface_height: u32,
    pub pill_height: f32,
    pub pill_top: f32,
    pub center_width: f32,
    pub right_width: f32,
    pub right_margin: f32,
    pub pill_color: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            strip_height: 36,
            surface_height: 320,
            pill_height: 30.0,
            pill_top: 3.0,
            center_width: 190.0,
            right_width: 120.0,
            right_margin: 8.0,
            pill_color: Color::BLACK,
        }
    }
}

/// A filled rounded rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pill {
    pub rect: Rect,
    pub radius: f32,
    pub color: Color,
}

/// Everything a renderer needs to draw one output's surface.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub pills: Vec<Pill>,
}

impl Scene {
    /// Rectangles that should receive pointer input; everything else falls through.
    pub fn input_rects(&self) -> impl Iterator<Item = Rect> + '_ {
        self.pills.iter().map(|p| p.rect)
    }
}

/// Positions of both islands on an output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub center: Rect,
    pub right: Rect,
}

impl Layout {
    pub fn compute(output_width: f32, theme: &Theme) -> Self {
        let h = theme.pill_height;
        let y = theme.pill_top;
        let center = Rect::new(
            (output_width - theme.center_width) / 2.0,
            y,
            theme.center_width,
            h,
        );
        let right = Rect::new(
            output_width - theme.right_margin - theme.right_width,
            y,
            theme.right_width,
            h,
        );
        Self { center, right }
    }

    pub fn scene(&self, theme: &Theme) -> Scene {
        let pill = |rect: Rect| Pill {
            rect,
            radius: rect.h / 2.0,
            color: theme.pill_color,
        };
        Scene {
            pills: vec![pill(self.center), pill(self.right)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_island_is_centered() {
        let theme = Theme::default();
        let l = Layout::compute(1645.0, &theme);
        let mid = l.center.x + l.center.w / 2.0;
        assert!((mid - 1645.0 / 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn right_island_respects_margin() {
        let theme = Theme::default();
        let l = Layout::compute(1000.0, &theme);
        assert_eq!(l.right.x + l.right.w, 1000.0 - theme.right_margin);
    }

    #[test]
    fn pills_fit_in_strip() {
        let theme = Theme::default();
        let l = Layout::compute(1000.0, &theme);
        for r in [l.center, l.right] {
            assert!(r.y >= 0.0 && r.y + r.h <= theme.strip_height as f32);
        }
    }

    #[test]
    fn round_out_covers_rect() {
        assert_eq!(Rect::new(1.5, 2.2, 3.0, 4.0).round_out(), (1, 2, 4, 5));
    }
}
