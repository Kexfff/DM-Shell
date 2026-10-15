//! Geometry, theme, scene description and island content layout. All coordinates are
//! logical pixels; renderers apply the output scale.
//!
//! Text is measured through the [`Measure`] trait so this module stays free of fonts;
//! the renderer implements it, tests use a fake.

use crate::activity::{ActivityView, Priority, View};

/// Axis-aligned rectangle in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
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

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Smallest integer rectangle covering this one, as `(x, y, w, h)`.
    pub fn round_out(&self) -> (i32, i32, i32, i32) {
        let x0 = self.x.floor() as i32;
        let y0 = self.y.floor() as i32;
        let x1 = self.right().ceil() as i32;
        let y1 = self.bottom().ceil() as i32;
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
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
    Bold,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub weight: Weight,
}

impl TextStyle {
    pub const fn new(size: f32, weight: Weight) -> Self {
        Self { size, weight }
    }

    pub fn line_height(&self) -> f32 {
        (self.size * 1.25).round()
    }
}

/// Lays out text. `max_lines > 1` wraps; overflow is ellipsized. Returns the laid-out
/// `(width, height)`.
pub trait Measure {
    fn measure(
        &mut self,
        text: &str,
        style: TextStyle,
        max_width: f32,
        max_lines: usize,
    ) -> (f32, f32);
}

/// Visual constants. The default is the Apple-like solid black pill theme.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Height reserved at the top of the output (layer-shell exclusive zone). Also the
    /// canvas height while nothing hangs below it.
    pub strip_height: u32,
    pub pill_height: f32,
    pub pill_top: f32,
    /// Corner radius cap; collapsed pills are fully round, expanded islands use this.
    pub max_radius: f32,
    pub center_idle_width: f32,
    pub center_min_width: f32,
    pub center_title_max: f32,
    pub center_expanded_width: f32,
    pub right_title_max: f32,
    pub right_expanded_width: f32,
    pub right_margin: f32,
    pub pill_color: Color,
    pub text: Color,
    pub text_secondary: Color,
    pub progress_track: Color,
    pub progress_fill: Color,
    /// Icon color per priority, low to critical.
    pub priority_colors: [Color; 4],
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            strip_height: 36,
            pill_height: 30.0,
            pill_top: 3.0,
            max_radius: 26.0,
            center_idle_width: 120.0,
            center_min_width: 160.0,
            center_title_max: 300.0,
            center_expanded_width: 400.0,
            right_title_max: 170.0,
            right_expanded_width: 340.0,
            right_margin: 8.0,
            pill_color: Color::BLACK,
            text: Color::rgba(245, 245, 247, 255),
            text_secondary: Color::rgba(152, 152, 157, 255),
            progress_track: Color::rgba(255, 255, 255, 45),
            progress_fill: Color::WHITE,
            // Apple system gray, white, orange, red.
            priority_colors: [
                Color::rgba(142, 142, 147, 255),
                Color::rgba(245, 245, 247, 255),
                Color::rgba(255, 159, 10, 255),
                Color::rgba(255, 69, 58, 255),
            ],
        }
    }
}

impl Theme {
    pub fn priority_color(&self, p: Priority) -> Color {
        self.priority_colors[p as usize]
    }

    /// Corner radius for an island of this size.
    pub fn radius(&self, rect: Rect) -> f32 {
        (rect.w.min(rect.h) / 2.0).min(self.max_radius)
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
    pub fn pill(rect: Rect, color: Color) -> Self {
        Self {
            rect,
            radius: rect.h.min(rect.w) / 2.0,
            color,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextDraw {
    pub text: String,
    pub style: TextStyle,
    pub color: Color,
    /// Top-left and layout box; `w` is the wrap/ellipsis width.
    pub rect: Rect,
    pub max_lines: usize,
    pub align: Align,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IconDraw {
    pub name: String,
    pub rect: Rect,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Draw {
    Rect(RoundRect),
    Text(TextDraw),
    Icon(IconDraw),
}

impl Draw {
    pub fn translate(&mut self, dx: f32, dy: f32) {
        let rect = match self {
            Draw::Rect(r) => &mut r.rect,
            Draw::Text(t) => &mut t.rect,
            Draw::Icon(i) => &mut i.rect,
        };
        *rect = rect.translate(dx, dy);
    }
}

/// Drawn together: clipped to `clip` (if any) and faded by `alpha`.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub clip: Option<Rect>,
    pub alpha: f32,
    pub items: Vec<Draw>,
}

/// Everything a renderer needs to draw one output's surface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    /// Drawn in order.
    pub groups: Vec<Group>,
    /// Island outlines; only these receive pointer input.
    pub islands: Vec<Rect>,
}

/// What an island shows, laid out at its natural size with items relative to (0, 0).
#[derive(Clone, Debug, PartialEq)]
pub struct Content {
    pub w: f32,
    pub h: f32,
    pub items: Vec<Draw>,
    /// Changes when the content is a different thing (not just an update), which
    /// triggers a cross-fade.
    pub key: String,
}

const PAD: f32 = 12.0;
const EXPANDED_PAD: f32 = 16.0;
const ROW_HEIGHT: f32 = 46.0;

/// Collapsed or expanded content for the center island.
pub fn center_content(view: &View, expanded: bool, theme: &Theme, m: &mut dyn Measure) -> Content {
    match (&view.center, expanded) {
        (None, false) => idle(view, theme, m),
        (None, true) => idle_expanded(view, theme, m),
        (Some(a), false) => activity_compact(a, theme, m),
        (Some(a), true) => activity_expanded(a, theme, m),
    }
}

/// Collapsed or expanded content for the right island; `None` hides it.
pub fn right_content(
    view: &View,
    expanded: bool,
    theme: &Theme,
    m: &mut dyn Measure,
) -> Option<Content> {
    let right = view.right.as_ref()?;
    Some(if expanded {
        right_list(&right.items, right.total, theme, m)
    } else {
        right_compact(right.newest(), right.total, theme, m)
    })
}

/// Measures `s` and returns a draw at (0, 0) plus its laid-out size.
fn text(
    m: &mut dyn Measure,
    s: &str,
    style: TextStyle,
    color: Color,
    max_w: f32,
    max_lines: usize,
) -> (TextDraw, f32, f32) {
    let (w, h) = m.measure(s, style, max_w, max_lines);
    let draw = TextDraw {
        text: s.to_owned(),
        style,
        color,
        rect: Rect::new(0.0, 0.0, max_w, h),
        max_lines,
        align: Align::Left,
    };
    (draw, w, h)
}

fn place(mut t: TextDraw, x: f32, y: f32) -> Draw {
    t.rect.x = x;
    t.rect.y = y;
    Draw::Text(t)
}

/// The activity's icon, or a status dot when it has none.
fn icon(a: &ActivityView, theme: &Theme, x: f32, y: f32, size: f32) -> Draw {
    let color = theme.priority_color(a.priority);
    match &a.icon {
        Some(name) => Draw::Icon(IconDraw {
            name: name.clone(),
            rect: Rect::new(x, y, size, size),
            color,
        }),
        None => {
            let d = size * 0.4;
            let dot = Rect::new(x + (size - d) / 2.0, y + (size - d) / 2.0, d, d);
            Draw::Rect(RoundRect::pill(dot, color))
        }
    }
}

fn bar(theme: &Theme, rect: Rect, progress: f32) -> [Draw; 2] {
    let fill = Rect {
        w: (rect.w * progress).max(rect.h),
        ..rect
    };
    let fill_color = if progress > 0.0 {
        theme.progress_fill
    } else {
        Color::TRANSPARENT
    };
    [
        Draw::Rect(RoundRect::pill(rect, theme.progress_track)),
        Draw::Rect(RoundRect::pill(fill, fill_color)),
    ]
}

fn idle(view: &View, theme: &Theme, m: &mut dyn Measure) -> Content {
    let h = theme.pill_height;
    let clock = view.clock.as_deref().unwrap_or("");
    let style = TextStyle::new(14.0, Weight::Semibold);
    let (t, tw, th) = text(m, clock, style, theme.text, 200.0, 1);
    let w = (tw + 2.0 * 18.0).max(theme.center_idle_width);
    Content {
        w,
        h,
        items: vec![place(t, (w - tw) / 2.0, (h - th) / 2.0)],
        key: "idle".into(),
    }
}

fn idle_expanded(view: &View, theme: &Theme, m: &mut dyn Measure) -> Content {
    let clock = view.clock.as_deref().unwrap_or("");
    let date = view.date.as_deref().unwrap_or("");
    let time_style = TextStyle::new(30.0, Weight::Bold);
    let (t, tw, th) = text(m, clock, time_style, theme.text, 300.0, 1);
    let date_style = TextStyle::new(13.0, Weight::Medium);
    let (d, dw, dh) = text(m, date, date_style, theme.text_secondary, 300.0, 1);
    let w = (tw.max(dw) + 2.0 * 28.0).max(220.0);
    let h = EXPANDED_PAD + th + 2.0 + dh + EXPANDED_PAD;
    Content {
        w,
        h,
        items: vec![
            place(t, (w - tw) / 2.0, EXPANDED_PAD),
            place(d, (w - dw) / 2.0, EXPANDED_PAD + th + 2.0),
        ],
        key: "idle:expanded".into(),
    }
}

fn activity_compact(a: &ActivityView, theme: &Theme, m: &mut dyn Measure) -> Content {
    let h = theme.pill_height;
    let icon_size = 16.0;
    let style = TextStyle::new(13.0, Weight::Semibold);
    let (t, tw, th) = text(m, &a.title, style, theme.text, theme.center_title_max, 1);

    let mut items = vec![icon(a, theme, PAD, (h - icon_size) / 2.0, icon_size)];
    let title_x = PAD + icon_size + 8.0;
    items.push(place(t, title_x, (h - th) / 2.0));
    let mut natural = title_x + tw + PAD;
    if let Some(p) = a.progress {
        let bar_rect = Rect::new(natural - PAD + 10.0, (h - 4.0) / 2.0, 44.0, 4.0);
        items.extend(bar(theme, bar_rect, p));
        natural = bar_rect.right() + PAD + 2.0;
    }
    // Keep the content centered when the minimum width kicks in.
    let w = natural.max(theme.center_min_width);
    let shift = (w - natural) / 2.0;
    items.iter_mut().for_each(|d| d.translate(shift, 0.0));
    Content {
        w,
        h,
        items,
        key: format!("a:{}", a.id),
    }
}

fn activity_expanded(a: &ActivityView, theme: &Theme, m: &mut dyn Measure) -> Content {
    let w = theme.center_expanded_width;
    let inner = w - 2.0 * EXPANDED_PAD;
    let icon_size = 22.0;
    let title_x = EXPANDED_PAD + icon_size + 10.0;
    let title_style = TextStyle::new(15.0, Weight::Semibold);
    let title_w = w - title_x - EXPANDED_PAD;
    let (t, _, th) = text(m, &a.title, title_style, theme.text, title_w, 1);

    let row = icon_size.max(th);
    let icon_y = EXPANDED_PAD + (row - icon_size) / 2.0;
    let mut items = vec![
        icon(a, theme, EXPANDED_PAD, icon_y, icon_size),
        place(t, title_x, EXPANDED_PAD + (row - th) / 2.0),
    ];
    let mut y = EXPANDED_PAD + row;
    if let Some(body) = &a.body {
        let style = TextStyle::new(13.0, Weight::Regular);
        let (b, _, bh) = text(m, body, style, theme.text_secondary, inner, 3);
        items.push(place(b, EXPANDED_PAD, y + 6.0));
        y += 6.0 + bh;
    }
    if let Some(p) = a.progress {
        items.extend(bar(theme, Rect::new(EXPANDED_PAD, y + 10.0, inner, 5.0), p));
        y += 10.0 + 5.0;
    }
    Content {
        w,
        h: y + EXPANDED_PAD,
        items,
        key: format!("a:{}:expanded", a.id),
    }
}

fn right_compact(a: &ActivityView, total: usize, theme: &Theme, m: &mut dyn Measure) -> Content {
    let h = theme.pill_height;
    let icon_size = 14.0;
    let style = TextStyle::new(12.5, Weight::Medium);
    let (t, tw, th) = text(m, &a.title, style, theme.text, theme.right_title_max, 1);
    let title_x = 10.0 + icon_size + 6.0;
    let mut items = vec![
        icon(a, theme, 10.0, (h - icon_size) / 2.0, icon_size),
        place(t, title_x, (h - th) / 2.0),
    ];
    let mut w = title_x + tw + PAD;
    if total > 1 {
        let style = TextStyle::new(11.0, Weight::Semibold);
        let badge = format!("+{}", total - 1);
        let (n, nw, nh) = text(m, &badge, style, theme.text_secondary, 60.0, 1);
        items.push(place(n, title_x + tw + 7.0, (h - nh) / 2.0));
        w += 7.0 + nw;
    }
    Content {
        w,
        h,
        items,
        key: format!("r:{}", a.id),
    }
}

fn right_list(items: &[ActivityView], total: usize, theme: &Theme, m: &mut dyn Measure) -> Content {
    let w = theme.right_expanded_width;
    let icon_size = 18.0;
    let text_x = 14.0 + icon_size + 12.0;
    let text_w = w - text_x - 14.0;
    let mut draws = Vec::new();
    let mut y = 8.0;
    for a in items {
        let title_style = TextStyle::new(13.0, Weight::Semibold);
        let (t, _, th) = text(m, &a.title, title_style, theme.text, text_w, 1);
        let icon_y = y + (ROW_HEIGHT - icon_size) / 2.0;
        draws.push(icon(a, theme, 14.0, icon_y, icon_size));
        match a.body.as_deref().filter(|b| !b.is_empty()) {
            Some(body) => {
                let body_style = TextStyle::new(12.0, Weight::Regular);
                let (b, _, bh) = text(m, body, body_style, theme.text_secondary, text_w, 1);
                let ty = y + (ROW_HEIGHT - th - bh) / 2.0;
                draws.push(place(t, text_x, ty));
                draws.push(place(b, text_x, ty + th));
            }
            None => draws.push(place(t, text_x, y + (ROW_HEIGHT - th) / 2.0)),
        }
        y += ROW_HEIGHT;
    }
    if total > items.len() {
        let style = TextStyle::new(12.0, Weight::Medium);
        let more = format!("+{} more", total - items.len());
        let (t, tw, th) = text(m, &more, style, theme.text_secondary, text_w, 1);
        draws.push(place(t, (w - tw) / 2.0, y + (24.0 - th) / 2.0));
        y += 24.0;
    }
    Content {
        w,
        h: y + 8.0,
        items: draws,
        key: "r:list".into(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::activity::RightView;

    /// Monospace-ish fake: each char is 0.55 em wide; wraps at `max_width`.
    pub struct FakeMeasure;

    impl Measure for FakeMeasure {
        fn measure(
            &mut self,
            text: &str,
            style: TextStyle,
            max_w: f32,
            max_lines: usize,
        ) -> (f32, f32) {
            let full = text.chars().count() as f32 * style.size * 0.55;
            let lines = ((full / max_w).ceil().max(1.0) as usize).min(max_lines);
            (full.min(max_w), lines as f32 * style.line_height())
        }
    }

    pub fn activity(id: &str, progress: Option<f32>) -> ActivityView {
        ActivityView {
            id: id.into(),
            title: format!("{id} title"),
            body: Some("body text".into()),
            progress,
            icon: Some("bell".into()),
            priority: Priority::Normal,
        }
    }

    pub fn view(center: Option<ActivityView>, right: &[ActivityView]) -> View {
        View {
            center,
            right: (!right.is_empty()).then(|| RightView {
                items: right.to_vec(),
                total: right.len(),
            }),
            clock: Some("13:35".into()),
            date: Some("Thursday, 15 October".into()),
        }
    }

    #[test]
    fn idle_shows_clock_centered() {
        let theme = Theme::default();
        let c = center_content(&view(None, &[]), false, &theme, &mut FakeMeasure);
        assert_eq!((c.w, c.h), (theme.center_idle_width, theme.pill_height));
        let Draw::Text(t) = &c.items[0] else {
            panic!("expected text")
        };
        assert_eq!(t.text, "13:35");
    }

    #[test]
    fn compact_grows_with_title_and_progress() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        // Long enough that the minimum width doesn't hide the differences.
        let a = ActivityView {
            title: "notification title".into(),
            ..activity("a", None)
        };
        let plain = center_content(&view(Some(a.clone()), &[]), false, &theme, &mut m);
        let long = ActivityView {
            title: "a much longer title for this one".into(),
            ..a.clone()
        };
        let longer = center_content(&view(Some(long), &[]), false, &theme, &mut m);
        let bar = ActivityView {
            progress: Some(0.5),
            ..a.clone()
        };
        let with_bar = center_content(&view(Some(bar), &[]), false, &theme, &mut m);
        assert!(longer.w > plain.w);
        assert!(with_bar.w > plain.w);
        assert_eq!(plain.h, theme.pill_height);
    }

    #[test]
    fn title_width_is_capped() {
        let theme = Theme::default();
        let long = ActivityView {
            title: "x".repeat(500),
            ..activity("a", None)
        };
        let c = center_content(&view(Some(long), &[]), false, &theme, &mut FakeMeasure);
        assert!(c.w <= theme.center_title_max + 2.0 * PAD + 16.0 + 8.0 + 0.5);
    }

    #[test]
    fn expanded_is_taller_and_keyed_differently() {
        let theme = Theme::default();
        let v = view(Some(activity("a", Some(0.3))), &[]);
        let compact = center_content(&v, false, &theme, &mut FakeMeasure);
        let expanded = center_content(&v, true, &theme, &mut FakeMeasure);
        assert!(expanded.h > compact.h);
        assert_ne!(compact.key, expanded.key);
    }

    #[test]
    fn right_compact_counts_and_list_rows() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let items = [activity("a", None), activity("b", None)];
        let compact = right_content(&view(None, &items), false, &theme, &mut m).unwrap();
        let single = right_content(&view(None, &items[..1]), false, &theme, &mut m).unwrap();
        assert!(compact.w > single.w, "+1 badge adds width");

        let list = right_content(&view(None, &items), true, &theme, &mut m).unwrap();
        assert_eq!(list.h, 8.0 + 2.0 * ROW_HEIGHT + 8.0);
        assert!(right_content(&view(None, &[]), false, &theme, &mut m).is_none());
    }

    #[test]
    fn radius_caps_for_tall_islands() {
        let theme = Theme::default();
        assert_eq!(theme.radius(Rect::new(0.0, 0.0, 200.0, 30.0)), 15.0);
        let tall = Rect::new(0.0, 0.0, 400.0, 200.0);
        assert_eq!(theme.radius(tall), theme.max_radius);
    }

    #[test]
    fn round_out_covers_rect() {
        assert_eq!(Rect::new(1.5, 2.2, 3.0, 4.0).round_out(), (1, 2, 4, 5));
    }
}
