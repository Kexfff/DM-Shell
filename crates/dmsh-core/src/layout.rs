//! Geometry, theme and island layout. All coordinates are logical pixels; renderers apply
//! the output scale.

use crate::activity::{ActivityView, Priority, View};

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
    pub const WHITE: Color = Color::rgba(255, 255, 255, 255);

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
    /// Center island width when idle and when showing an activity.
    pub center_idle_width: f32,
    pub center_active_width: f32,
    pub right_width: f32,
    pub right_margin: f32,
    pub pill_color: Color,
    pub progress_track: Color,
    pub progress_fill: Color,
    /// Status dot color per priority, low to critical.
    pub priority_colors: [Color; 4],
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            strip_height: 36,
            surface_height: 320,
            pill_height: 30.0,
            pill_top: 3.0,
            center_idle_width: 190.0,
            center_active_width: 300.0,
            right_width: 120.0,
            right_margin: 8.0,
            pill_color: Color::BLACK,
            progress_track: Color::rgba(255, 255, 255, 50),
            progress_fill: Color::WHITE,
            // Apple system gray, white, orange, red.
            priority_colors: [
                Color::rgba(142, 142, 147, 255),
                Color::WHITE,
                Color::rgba(255, 159, 10, 255),
                Color::rgba(255, 69, 58, 255),
            ],
        }
    }
}

impl Theme {
    fn priority_color(&self, p: Priority) -> Color {
        self.priority_colors[p as usize]
    }
}

/// A filled rounded rectangle (a circle when the radius is half the size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundRect {
    pub rect: Rect,
    pub radius: f32,
    pub color: Color,
}

impl RoundRect {
    fn pill(rect: Rect, color: Color) -> Self {
        Self {
            rect,
            radius: rect.h.min(rect.w) / 2.0,
            color,
        }
    }

    fn circle(cx: f32, cy: f32, r: f32, color: Color) -> Self {
        Self::pill(Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r), color)
    }
}

/// Everything a renderer needs to draw one output's surface.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    /// Drawn in order.
    pub shapes: Vec<RoundRect>,
    /// Island outlines; only these receive pointer input.
    pub islands: Vec<Rect>,
}

/// Positions of the islands on an output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub center: Rect,
    /// `None` when the right island has nothing to show.
    pub right: Option<Rect>,
}

impl Layout {
    pub fn compute(output_width: f32, theme: &Theme, view: &View) -> Self {
        let (h, y) = (theme.pill_height, theme.pill_top);
        let cw = match view.center {
            Some(_) => theme.center_active_width,
            None => theme.center_idle_width,
        };
        let center = Rect::new((output_width - cw) / 2.0, y, cw, h);
        let right = view.right.as_ref().map(|_| {
            let x = output_width - theme.right_margin - theme.right_width;
            Rect::new(x, y, theme.right_width, h)
        });
        Self { center, right }
    }

    /// Placeholder content until text lands: a priority dot, a progress bar, and one
    /// small dot per extra item waiting on the right island (up to three).
    pub fn scene(&self, theme: &Theme, view: &View) -> Scene {
        let mut shapes = vec![RoundRect::pill(self.center, theme.pill_color)];
        let mut islands = vec![self.center];

        if let Some(activity) = &view.center {
            content(self.center, activity, theme, &mut shapes);
        }
        if let (Some(rect), Some(right)) = (self.right, &view.right) {
            islands.push(rect);
            shapes.push(RoundRect::pill(rect, theme.pill_color));
            content(rect, &right.newest, theme, &mut shapes);
            let dim = Color {
                a: 110,
                ..Color::WHITE
            };
            for i in 0..right.more.min(3) {
                let cx = rect.x + rect.w - rect.h / 2.0 - i as f32 * 8.0;
                shapes.push(RoundRect::circle(cx, rect.y + rect.h / 2.0, 2.5, dim));
            }
        }
        Scene { shapes, islands }
    }
}

fn content(island: Rect, activity: &ActivityView, theme: &Theme, shapes: &mut Vec<RoundRect>) {
    let r = island.h / 2.0;
    let color = theme.priority_color(activity.priority);
    shapes.push(RoundRect::circle(island.x + r, island.y + r, 4.0, color));

    if let Some(progress) = activity.progress {
        let track = Rect::new(
            island.x + r + 10.0,
            island.y + r - 1.5,
            island.w - 2.0 * r - 20.0,
            3.0,
        );
        shapes.push(RoundRect::pill(track, theme.progress_track));
        if progress > 0.0 {
            let fill = Rect {
                w: (track.w * progress).max(track.h),
                ..track
            };
            shapes.push(RoundRect::pill(fill, theme.progress_fill));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::RightView;

    fn activity(progress: Option<f32>) -> ActivityView {
        ActivityView {
            id: "a".into(),
            title: "a".into(),
            body: None,
            progress,
            priority: Priority::Normal,
        }
    }

    #[test]
    fn center_island_is_centered_and_grows_when_active() {
        let theme = Theme::default();
        let idle = Layout::compute(1645.0, &theme, &View::default());
        let active_view = View {
            center: Some(activity(None)),
            right: None,
        };
        let active = Layout::compute(1645.0, &theme, &active_view);
        for l in [idle, active] {
            let mid = l.center.x + l.center.w / 2.0;
            assert!((mid - 1645.0 / 2.0).abs() < 0.001);
        }
        assert!(active.center.w > idle.center.w);
    }

    #[test]
    fn right_island_hidden_when_empty() {
        let theme = Theme::default();
        let l = Layout::compute(1000.0, &theme, &View::default());
        assert!(l.right.is_none());
        assert_eq!(l.scene(&theme, &View::default()).islands.len(), 1);
    }

    #[test]
    fn right_island_respects_margin_and_strip() {
        let theme = Theme::default();
        let view = View {
            center: None,
            right: Some(RightView {
                newest: activity(None),
                more: 0,
            }),
        };
        let right = Layout::compute(1000.0, &theme, &view).right.unwrap();
        assert_eq!(right.x + right.w, 1000.0 - theme.right_margin);
        assert!(right.y >= 0.0 && right.y + right.h <= theme.strip_height as f32);
    }

    #[test]
    fn progress_fill_stays_inside_track() {
        let theme = Theme::default();
        let view = View {
            center: Some(activity(Some(1.0))),
            right: None,
        };
        let l = Layout::compute(1000.0, &theme, &view);
        let shapes = l.scene(&theme, &view).shapes;
        let (track, fill) = (shapes[2].rect, shapes[3].rect);
        assert_eq!(fill.x + fill.w, track.x + track.w);
    }

    #[test]
    fn round_out_covers_rect() {
        assert_eq!(Rect::new(1.5, 2.2, 3.0, 4.0).round_out(), (1, 2, 4, 5));
    }
}
