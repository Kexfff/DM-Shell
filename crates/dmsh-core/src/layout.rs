//! Geometry, theme, scene description and island content layout. All coordinates are
//! logical pixels; renderers apply the output scale.
//!
//! Text is measured through the [`Measure`] trait so this module stays free of fonts;
//! the renderer implements it, tests use a fake.

use crate::activity::{
    Action, ActivityView, BatteryLevel, BatteryView, Control, Controls, DEFAULT_ACTION, Level,
    MenuView, Priority, SessionAction, Slider, TrayIconView, View,
};

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
#[derive(Clone, Debug, PartialEq)]
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
    pub button: Color,
    pub highlight: Color,
    /// Battery icon while charging.
    pub charging: Color,
    /// Icon color per priority, low to critical.
    pub priority_colors: [Color; 4],
    pub motion: Motion,
}

/// Spring animation settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    /// `false` snaps instead of animating.
    pub enabled: bool,
    /// Seconds per oscillation.
    pub response: f32,
    /// 1.0 = no overshoot.
    pub damping: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            enabled: true,
            response: 0.42,
            damping: 0.78,
        }
    }
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
            button: Color::rgba(255, 255, 255, 30),
            highlight: Color::rgba(255, 255, 255, 22),
            charging: Color::rgba(52, 199, 89, 255),
            // Apple system gray, white, orange, red.
            priority_colors: [
                Color::rgba(142, 142, 147, 255),
                Color::rgba(245, 245, 247, 255),
                Color::rgba(255, 159, 10, 255),
                Color::rgba(255, 69, 58, 255),
            ],
            motion: Motion::default(),
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

/// A bitmap or SVG registered with the renderer under `key` (tray icons).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageDraw {
    pub key: String,
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Draw {
    Rect(RoundRect),
    Text(TextDraw),
    Icon(IconDraw),
    Image(ImageDraw),
}

impl Draw {
    pub fn translate(&mut self, dx: f32, dy: f32) {
        let rect = match self {
            Draw::Rect(r) => &mut r.rect,
            Draw::Text(t) => &mut t.rect,
            Draw::Icon(i) => &mut i.rect,
            Draw::Image(i) => &mut i.rect,
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

/// What clicking somewhere does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Invoke {
        id: String,
        key: String,
    },
    Dismiss {
        id: String,
    },
    ClearRight,
    /// A tray icon; the shell picks activate / menu by mouse button.
    Tray {
        id: String,
    },
    MenuItem {
        id: i32,
    },
    MenuBack,
    Control(Control),
    /// Open (`true`) or close the session menu.
    SessionMenu(bool),
    /// A session menu option: runs when held down long enough, not on a click.
    Session(SessionAction),
    /// The volume or brightness button: a click opens its slider (holding the volume
    /// one mutes instead).
    Level(Slider),
    /// The slider itself: press or drag to set, or scroll.
    Slider(Slider),
    /// Back from the slider to the buttons.
    CloseSlider,
    /// Scroll the expanded right island's list by this many rows.
    Scroll(i32),
}

/// A clickable area; the first hit containing the pointer wins.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub rect: Rect,
    pub target: Target,
    /// Show a hover highlight (buttons, list rows), not for whole-island clicks.
    pub highlight: bool,
    /// Reachable with keyboard navigation (small duplicates like close buttons aren't).
    pub keyboard: bool,
}

/// What an island shows, laid out at its natural size with items relative to (0, 0).
#[derive(Clone, Debug, PartialEq)]
pub struct Content {
    pub w: f32,
    pub h: f32,
    pub items: Vec<Draw>,
    pub hits: Vec<Hit>,
    /// Changes when the content is a different thing (not just an update), which
    /// triggers a cross-fade.
    pub key: String,
}

const PAD: f32 = 12.0;
const EXPANDED_PAD: f32 = 16.0;
const ROW_HEIGHT: f32 = 46.0;
/// Notification rows visible at once in the expanded right island.
pub const LIST_ROWS: usize = 6;
/// Menu entries visible at once; longer tray menus scroll.
pub const MENU_ROWS: usize = 12;
/// Diameter of the round quick-control buttons.
const CONTROL_SIZE: f32 = 36.0;
/// Space between them.
const CONTROL_GAP: f32 = 6.0;

/// Collapsed or expanded content for the center island.
pub fn center_content(view: &View, expanded: bool, theme: &Theme, m: &mut dyn Measure) -> Content {
    match (&view.center, expanded) {
        (None, false) => idle(view, theme, m),
        (None, true) => idle_expanded(view, theme, m),
        (Some(a), false) => activity_compact(a, clock_beside(view, a), theme, m),
        (Some(a), true) => activity_expanded(a, clock_beside(view, a), theme, m),
    }
}

/// Collapsed or expanded content for the right island; `None` hides it.
pub fn right_content(
    view: &View,
    expanded: bool,
    theme: &Theme,
    m: &mut dyn Measure,
) -> Option<Content> {
    if let Some(menu) = &view.menu {
        return Some(menu_list(menu, theme, m));
    }
    let empty = view.right.is_none()
        && view.tray.is_empty()
        && view.battery.is_none()
        && view.weather.is_none()
        && !view.privacy.any();
    match expanded {
        // Expanded always has the quick controls, even with nothing else to show.
        true => Some(right_list(view, theme, m)),
        false if empty => None,
        false => Some(right_compact(view, theme, m)),
    }
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
    if let Some(key) = &a.image {
        return Draw::Image(ImageDraw {
            key: key.clone(),
            rect: Rect::new(x, y, size, size),
        });
    }
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
        hits: Vec::new(),
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
        hits: Vec::new(),
        key: "idle:expanded".into(),
    }
}

/// The clock, when it should show next to `a`: beside lasting activities like media, so
/// the time stays in view; brief ones (an OSD, a new notification) cover it.
fn clock_beside<'v>(view: &'v View, a: &ActivityView) -> Option<&'v str> {
    view.clock.as_deref().filter(|_| a.lasting)
}

fn activity_compact(
    a: &ActivityView,
    clock: Option<&str>,
    theme: &Theme,
    m: &mut dyn Measure,
) -> Content {
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
    if let Some(clock) = clock {
        let style = TextStyle::new(13.0, Weight::Semibold);
        let (c, cw, ch) = text(m, clock, style, theme.text_secondary, 120.0, 1);
        let x = natural - PAD + 14.0;
        items.push(place(c, x, (h - ch) / 2.0));
        natural = x + cw + PAD + 2.0;
    }
    // Keep the content centered when the minimum width kicks in.
    let w = natural.max(theme.center_min_width);
    let shift = (w - natural) / 2.0;
    items.iter_mut().for_each(|d| d.translate(shift, 0.0));
    Content {
        w,
        h,
        items,
        hits: default_hit(a, w, h).into_iter().collect(),
        key: format!("a:{}", a.id),
    }
}

fn activity_expanded(
    a: &ActivityView,
    clock: Option<&str>,
    theme: &Theme,
    m: &mut dyn Measure,
) -> Content {
    let w = theme.center_expanded_width;
    let inner = w - 2.0 * EXPANDED_PAD;
    let icon_size = 22.0;
    let title_x = EXPANDED_PAD + icon_size + 10.0;
    let mut items = Vec::new();
    // The clock in the top right corner, beside the caption (or the title).
    let mut clock_w = 0.0;
    if let Some(clock) = clock {
        let style = TextStyle::new(12.5, Weight::Semibold);
        let (c, cw, _) = text(m, clock, style, theme.text_secondary, 120.0, 1);
        items.push(place(c, w - EXPANDED_PAD - cw, EXPANDED_PAD - 2.0));
        clock_w = cw + 12.0;
    }
    let title_style = TextStyle::new(15.0, Weight::Semibold);
    let title_w = w - title_x - EXPANDED_PAD;
    let has_caption = a.app.as_deref().is_some_and(|s| !s.is_empty());
    let first_line_w = |full: f32| full - clock_w;
    let title_max = if has_caption {
        title_w
    } else {
        first_line_w(title_w)
    };
    let (t, _, th) = text(m, &a.title, title_style, theme.text, title_max, 1);

    // Who it's from, as a small caption above the title.
    let mut top = EXPANDED_PAD;
    if let Some(app) = a.app.as_deref().filter(|s| !s.is_empty()) {
        let style = TextStyle::new(11.5, Weight::Semibold);
        let (c, _, ch) = text(
            m,
            app,
            style,
            theme.text_secondary,
            first_line_w(title_w),
            1,
        );
        items.push(place(c, title_x, top - 2.0));
        top += ch;
    }
    let row = icon_size.max(th);
    let icon_y = top + (row - icon_size) / 2.0;
    items.push(icon(a, theme, EXPANDED_PAD, icon_y, icon_size));
    items.push(place(t, title_x, top + (row - th) / 2.0));
    let mut y = top + row;
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
    let mut hits = Vec::new();
    let buttons: Vec<&Action> = a
        .actions
        .iter()
        .filter(|b| b.key != DEFAULT_ACTION)
        .collect();
    if !buttons.is_empty() {
        y += 12.0;
        y += action_row(
            a,
            &buttons,
            theme,
            m,
            EXPANDED_PAD,
            y,
            inner,
            &mut items,
            &mut hits,
        );
    }
    let h = y + EXPANDED_PAD;
    hits.extend(default_hit(a, w, h));
    Content {
        w,
        h,
        items,
        hits,
        key: format!("a:{}:expanded", a.id),
    }
}

/// The newest item with a `+N` badge, then the tray icons, then the battery.
/// Icons for what is capturing the user, in their colors: screen red, microphone orange,
/// camera green.
fn privacy_icons(view: &View, theme: &Theme) -> Vec<(&'static str, Color)> {
    let p = view.privacy;
    [
        (
            p.screen,
            "screen-share",
            theme.priority_color(Priority::Critical),
        ),
        (p.mic, "mic", theme.priority_color(Priority::High)),
        (p.camera, "camera", theme.charging),
    ]
    .into_iter()
    .filter_map(|(on, icon, color)| on.then_some((icon, color)))
    .collect()
}

fn right_compact(view: &View, theme: &Theme, m: &mut dyn Measure) -> Content {
    let h = theme.pill_height;
    let mut items = Vec::new();
    let mut hits = Vec::new();
    let mut x = 10.0;
    let mut end_pad = PAD;
    // Capture indicators first, where they're easiest to notice.
    let privacy = privacy_icons(view, theme);
    for (name, color) in &privacy {
        let size = 14.0;
        items.push(Draw::Icon(IconDraw {
            name: (*name).into(),
            rect: Rect::new(x, (h - size) / 2.0, size, size),
            color: *color,
        }));
        x += size + 6.0;
    }
    if !privacy.is_empty() {
        x += 4.0;
    }
    let after_privacy = x;
    if let Some(right) = &view.right {
        let a = right.newest();
        let icon_size = 14.0;
        let style = TextStyle::new(12.5, Weight::Medium);
        let (t, tw, th) = text(m, &a.title, style, theme.text, theme.right_title_max, 1);
        // "Microphone in use" right after it starts: the indicator already shows its icon.
        let announced = a
            .icon
            .as_deref()
            .is_some_and(|i| privacy.iter().any(|(name, _)| *name == i));
        let title_x = if announced {
            x - 4.0
        } else {
            items.push(icon(a, theme, x, (h - icon_size) / 2.0, icon_size));
            x + icon_size + 6.0
        };
        items.push(place(t, title_x, (h - th) / 2.0));
        x = title_x + tw;
        if right.total > 1 {
            let style = TextStyle::new(11.0, Weight::Semibold);
            let badge = format!("+{}", right.total - 1);
            let (n, nw, nh) = text(m, &badge, style, theme.text_secondary, 60.0, 1);
            items.push(place(n, x + 7.0, (h - nh) / 2.0));
            x += 7.0 + nw;
        }
        x += 10.0;
    }
    let slot = 26.0;
    for item in &view.tray {
        let rect = Rect::new(x, (h - slot) / 2.0, slot, slot);
        items.push(tray_icon(item, theme, rect, 16.0));
        hits.push(Hit {
            rect,
            target: Target::Tray {
                id: item.id.clone(),
            },
            highlight: true,
            keyboard: true,
        });
        x += slot;
        end_pad = 4.0;
    }
    if view.controls.dnd {
        if !view.tray.is_empty() {
            x += 4.0;
        }
        let size = 14.0;
        items.push(Draw::Icon(IconDraw {
            name: "moon".into(),
            rect: Rect::new(x, (h - size) / 2.0, size, size),
            color: theme.text_secondary,
        }));
        x += size + 8.0;
        end_pad = PAD;
    }
    if let Some(temperature) = &view.weather {
        if !view.tray.is_empty() && !view.controls.dnd {
            x += 4.0;
        }
        let style = TextStyle::new(12.0, Weight::Semibold);
        let (t, tw, th) = text(m, temperature, style, theme.text, 60.0, 1);
        items.push(place(t, x, (h - th) / 2.0));
        x += tw + 10.0;
        end_pad = PAD - 10.0;
    }
    if let Some(battery) = &view.battery {
        if !view.tray.is_empty() && !view.controls.dnd && view.weather.is_none() {
            x += 4.0;
        }
        let size = 20.0;
        let icon_rect = Rect::new(x, (h - size) / 2.0, size, size);
        items.extend(battery_icon(battery, theme, icon_rect));
        let style = TextStyle::new(12.0, Weight::Semibold);
        let label = format!("{}%", battery.percent);
        let (t, tw, th) = text(m, &label, style, theme.text, 60.0, 1);
        items.push(place(t, x + size + 3.0, (h - th) / 2.0));
        x += size + 3.0 + tw;
        end_pad = PAD;
    }
    if !privacy.is_empty() && x == after_privacy {
        // Only the indicators: no gap after the last one.
        x -= 10.0;
    }
    let key = match &view.right {
        Some(r) => format!("r:{}", r.newest().id),
        None => "r:status".into(),
    };
    Content {
        w: x + end_pad,
        h,
        items,
        hits,
        key,
    }
}

/// A battery outline with a fill for the charge (or a bolt while charging); the fill is
/// orange when low and red when critical, the icon green while charging.
fn battery_icon(b: &BatteryView, theme: &Theme, rect: Rect) -> Vec<Draw> {
    let level_color = match b.level {
        BatteryLevel::Normal => theme.text,
        BatteryLevel::Low => theme.priority_color(Priority::High),
        BatteryLevel::Critical => theme.priority_color(Priority::Critical),
    };
    let (name, color) = if b.charging {
        ("battery-charging", theme.charging)
    } else {
        ("battery", theme.text_secondary)
    };
    let mut draws = vec![Draw::Icon(IconDraw {
        name: name.into(),
        rect,
        color,
    })];
    if !b.charging {
        // Inside the icon's body (24-unit grid: x 5..16, y 9.5..14.5).
        let u = rect.w / 24.0;
        let fill = Rect::new(
            rect.x + 5.0 * u,
            rect.y + 9.5 * u,
            (11.0 * u * f32::from(b.percent.min(100)) / 100.0).max(u),
            5.0 * u,
        );
        draws.push(Draw::Rect(RoundRect {
            rect: fill,
            radius: u,
            color: level_color,
        }));
    }
    draws
}

/// A round, icon-only quick-control button: filled when on.
fn control_button(
    icon_name: &str,
    on: bool,
    rect: Rect,
    control: Control,
    theme: &Theme,
    draws: &mut Vec<Draw>,
    hits: &mut Vec<Hit>,
) {
    let (bg, fg) = if on {
        (theme.text, theme.pill_color)
    } else {
        (theme.button, theme.text)
    };
    draws.push(Draw::Rect(RoundRect::pill(rect, bg)));
    let size = 18.0;
    draws.push(Draw::Icon(IconDraw {
        name: icon_name.into(),
        rect: Rect::new(
            rect.x + (rect.w - size) / 2.0,
            rect.y + (rect.h - size) / 2.0,
            size,
            size,
        ),
        color: fg,
    }));
    hits.push(Hit {
        rect,
        target: Target::Control(control),
        highlight: !on,
        keyboard: true,
    });
}

/// A round, icon-only button that isn't a toggle.
fn round_button(
    icon_name: &str,
    rect: Rect,
    target: Target,
    theme: &Theme,
    draws: &mut Vec<Draw>,
    hits: &mut Vec<Hit>,
) {
    draws.push(Draw::Rect(RoundRect::pill(rect, theme.button)));
    let size = 18.0;
    draws.push(Draw::Icon(IconDraw {
        name: icon_name.into(),
        rect: Rect::new(
            rect.x + (rect.w - size) / 2.0,
            rect.y + (rect.h - size) / 2.0,
            size,
            size,
        ),
        color: theme.text,
    }));
    hits.push(Hit {
        rect,
        target,
        highlight: true,
        keyboard: true,
    });
}

/// The session menu in place of the controls row: a back button, then one pill per
/// option that fills up while it's held.
fn session_row(
    controls: &Controls,
    y: f32,
    w: f32,
    theme: &Theme,
    m: &mut dyn Measure,
    draws: &mut Vec<Draw>,
    hits: &mut Vec<Hit>,
) {
    let (size, gap, pad) = (CONTROL_SIZE, CONTROL_GAP, 10.0);
    let back = Rect::new(pad, y, size, size);
    round_button(
        "chevron-left",
        back,
        Target::SessionMenu(false),
        theme,
        draws,
        hits,
    );
    let n = SessionAction::ALL.len() as f32;
    let x0 = back.right() + gap;
    let pill_w = (w - pad - x0 - gap * (n - 1.0)) / n;
    let fill = theme.priority_color(Priority::Critical);
    for (i, action) in SessionAction::ALL.into_iter().enumerate() {
        let rect = Rect::new(x0 + i as f32 * (pill_w + gap), y, pill_w, size);
        draws.push(Draw::Rect(RoundRect::pill(rect, theme.button)));
        if let Some((held, progress)) = controls.session_hold
            && held == action
            && progress > 0.0
        {
            // Starts as a circle at the left end (fading in) and grows to the full pill.
            let p = progress.min(1.0);
            let alpha = (fill.a as f32 * (p * 6.0).min(1.0)) as u8;
            let grown = Rect::new(rect.x, rect.y, size + (rect.w - size) * p, size);
            draws.push(Draw::Rect(RoundRect::pill(
                grown,
                Color { a: alpha, ..fill },
            )));
        }
        let style = TextStyle::new(12.5, Weight::Semibold);
        let (t, tw, th) = text(m, action.label(), style, theme.text, pill_w - 12.0, 1);
        draws.push(place(
            t,
            rect.x + (rect.w - tw.min(pill_w - 12.0)) / 2.0,
            y + (size - th) / 2.0,
        ));
        hits.push(Hit {
            rect,
            target: Target::Session(action),
            highlight: true,
            keyboard: true,
        });
    }
}

fn level_icon(slider: Slider, level: Level) -> &'static str {
    match slider {
        Slider::Volume if level.muted || level.value <= 0.0 => "volume-mute",
        Slider::Volume => "volume",
        Slider::Mic if level.muted || level.value <= 0.0 => "mic-off",
        Slider::Mic => "mic",
        Slider::Brightness => "brightness",
    }
}

/// The open volume or brightness slider in place of the controls row: back button,
/// track, and the level as text.
fn slider_row(
    (slider, level): (Slider, Level),
    y: f32,
    w: f32,
    theme: &Theme,
    m: &mut dyn Measure,
    draws: &mut Vec<Draw>,
    hits: &mut Vec<Hit>,
) {
    let (size, gap, pad) = (CONTROL_SIZE, CONTROL_GAP, 10.0);
    let back = Rect::new(pad, y, size, size);
    round_button(
        "chevron-left",
        back,
        Target::CloseSlider,
        theme,
        draws,
        hits,
    );
    // The level as text on the right; the track gets the rest.
    let muted = slider != Slider::Brightness && level.muted;
    let label = if muted {
        "Muted".to_owned()
    } else {
        format!("{}%", (level.value * 100.0).round() as i32)
    };
    let style = TextStyle::new(12.5, Weight::Semibold);
    let (t, tw, th) = text(m, &label, style, theme.text, 80.0, 1);
    draws.push(place(t, w - pad - tw, y + (size - th) / 2.0));
    // Room for "100%" at least, so the track doesn't jump as the level changes.
    let label_w = tw.max(text(m, "100%", style, theme.text, 80.0, 1).1);
    let track = Rect::new(
        back.right() + gap,
        y,
        w - pad - label_w - 10.0 - back.right() - gap,
        size,
    );
    draws.push(Draw::Rect(RoundRect::pill(track, theme.button)));
    // The fill starts as a circle around the icon and grows to the full track.
    let fill_color = if muted {
        theme.text_secondary
    } else {
        theme.text
    };
    let fill_w = size + (track.w - size) * level.value.clamp(0.0, 1.0);
    let fill = Rect::new(track.x, y, fill_w, size);
    draws.push(Draw::Rect(RoundRect::pill(fill, fill_color)));
    let icon = 18.0;
    draws.push(Draw::Icon(IconDraw {
        name: level_icon(slider, level).into(),
        rect: Rect::new(
            track.x + (size - icon) / 2.0,
            y + (size - icon) / 2.0,
            icon,
            icon,
        ),
        color: theme.pill_color,
    }));
    hits.push(Hit {
        rect: track,
        target: Target::Slider(slider),
        highlight: false,
        keyboard: true,
    });
}

/// Where on a slider track `x` falls, 0.0 to 1.0 (the icon circle at the left is 0).
pub fn slider_value(track: Rect, x: f32) -> f32 {
    let size = track.h;
    ((x - track.x - size / 2.0) / (track.w - size).max(1.0)).clamp(0.0, 1.0)
}

/// Round icon buttons for the quick controls, from the left at `y`; returns where they end.
fn controls_row(
    controls: &Controls,
    y: f32,
    theme: &Theme,
    draws: &mut Vec<Draw>,
    hits: &mut Vec<Hit>,
) -> f32 {
    let mut buttons: Vec<(&str, bool, Control)> = Vec::new();
    if let Some(on) = controls.wifi {
        let icon = if on { "wifi" } else { "wifi-off" };
        buttons.push((icon, on, Control::Wifi));
    }
    buttons.push(("moon", controls.dnd, Control::Dnd));
    buttons.push(("coffee", controls.keep_awake, Control::KeepAwake));
    let (size, gap, pad) = (CONTROL_SIZE, CONTROL_GAP, 10.0);
    let mut x = pad;
    for (icon, on, control) in buttons {
        let rect = Rect::new(x, y, size, size);
        control_button(icon, on, rect, control, theme, draws, hits);
        x += size + gap;
    }
    // Volume and brightness open a slider in place of this row.
    let levels = [
        (Slider::Volume, controls.volume),
        (Slider::Mic, controls.mic),
        (Slider::Brightness, controls.brightness),
    ];
    for (slider, level) in levels {
        let Some(level) = level else { continue };
        let rect = Rect::new(x, y, size, size);
        let icon = level_icon(slider, level);
        round_button(icon, rect, Target::Level(slider), theme, draws, hits);
        x += size + gap;
    }
    x - gap
}

/// A tray icon centered in `slot`, or a dot when the app gave no usable icon.
fn tray_icon(item: &TrayIconView, theme: &Theme, slot: Rect, size: f32) -> Draw {
    let rect = Rect::new(
        slot.x + (slot.w - size) / 2.0,
        slot.y + (slot.h - size) / 2.0,
        size,
        size,
    );
    match &item.image {
        Some(key) => Draw::Image(ImageDraw {
            key: key.clone(),
            rect,
        }),
        None => {
            let d = size * 0.35;
            let dot = Rect::new(rect.x + (size - d) / 2.0, rect.y + (size - d) / 2.0, d, d);
            Draw::Rect(RoundRect::pill(dot, theme.text_secondary))
        }
    }
}

/// An open tray menu: header, optional back row, entries.
fn menu_list(menu: &MenuView, theme: &Theme, m: &mut dyn Measure) -> Content {
    // As wide as the expanded island it replaces: narrower, the island would shrink out
    // from under the pointer (on the tray icon) and collapse.
    let w = theme.right_expanded_width.max(300.0);
    let row = 32.0;
    let label_x = 36.0;
    let label_w = w - label_x - 34.0;
    let mut items = Vec::new();
    let mut hits = Vec::new();
    let mut y = 6.0;
    if !menu.title.is_empty() {
        let style = TextStyle::new(12.0, Weight::Semibold);
        let (t, _, th) = text(m, &menu.title, style, theme.text_secondary, w - 28.0, 1);
        items.push(place(t, 14.0, y + (26.0 - th) / 2.0));
        y += 26.0;
    }
    let entry_row =
        |label: &str, color: Color, y: f32, items: &mut Vec<Draw>, m: &mut dyn Measure| {
            let style = TextStyle::new(13.0, Weight::Regular);
            let (t, _, th) = text(m, label, style, color, label_w, 1);
            items.push(place(t, label_x, y + (row - th) / 2.0));
        };
    let small_icon = |name: &str, x: f32, y: f32, color: Color| {
        Draw::Icon(IconDraw {
            name: name.into(),
            rect: Rect::new(x, y + (row - 14.0) / 2.0, 14.0, 14.0),
            color,
        })
    };
    if menu.nested {
        items.push(small_icon("chevron-left", 14.0, y, theme.text_secondary));
        entry_row("Back", theme.text_secondary, y, &mut items, m);
        hits.push(Hit {
            rect: Rect::new(6.0, y, w - 12.0, row),
            target: Target::MenuBack,
            highlight: true,
            keyboard: true,
        });
        y += row;
    }
    let offset = menu
        .offset
        .min(menu.entries.len().saturating_sub(MENU_ROWS));
    if offset > 0 {
        let delta = Some(-(MENU_ROWS as i32));
        let label = format!("{offset} above");
        scroll_row(
            &label,
            "chevron-up",
            delta,
            y,
            w,
            theme,
            m,
            &mut items,
            &mut hits,
        );
        y += 28.0;
    }
    let shown = &menu.entries[offset..(offset + MENU_ROWS).min(menu.entries.len())];
    let below = menu.entries.len() - offset - shown.len();
    for entry in shown {
        if entry.separator {
            let line = Rect::new(14.0, y + 4.0, w - 28.0, 1.0);
            items.push(Draw::Rect(RoundRect {
                rect: line,
                radius: 0.0,
                color: theme.button,
            }));
            y += 9.0;
            continue;
        }
        let color = if entry.enabled {
            theme.text
        } else {
            theme.text_secondary
        };
        if entry.toggle == Some(true) {
            items.push(small_icon("check", 14.0, y, color));
        }
        entry_row(&entry.label, color, y, &mut items, m);
        if entry.submenu {
            items.push(small_icon(
                "chevron-right",
                w - 28.0,
                y,
                theme.text_secondary,
            ));
        }
        if entry.enabled {
            hits.push(Hit {
                rect: Rect::new(6.0, y, w - 12.0, row),
                target: Target::MenuItem { id: entry.id },
                highlight: true,
                keyboard: true,
            });
        }
        y += row;
    }
    if below > 0 {
        let delta = Some(MENU_ROWS as i32);
        let label = format!("{below} more");
        scroll_row(
            &label,
            "chevron-down",
            delta,
            y,
            w,
            theme,
            m,
            &mut items,
            &mut hits,
        );
        y += 28.0;
    }
    let first = menu.entries.first().map_or(0, |e| e.id);
    Content {
        w,
        h: y + 6.0,
        items,
        hits,
        key: format!("menu:{}:{first}", menu.title),
    }
}

/// Tray icons and battery, quick controls, then one row per item with a close button,
/// then "Clear all".
fn right_list(view: &View, theme: &Theme, m: &mut dyn Measure) -> Content {
    let tray = &view.tray;
    let (items, total): (&[ActivityView], usize) = match &view.right {
        Some(right) => (&right.items, right.total),
        None => (&[], 0),
    };
    let w = theme.right_expanded_width;
    let icon_size = 18.0;
    let close = 28.0;
    let text_x = 14.0 + icon_size + 12.0;
    let text_w = w - text_x - 8.0 - close - 6.0;
    let mut draws = Vec::new();
    let mut hits = Vec::new();
    // Header: quick controls on the left, battery on the right, tray icons between them
    // (on their own row when they don't fit).
    let mut y = 10.0;
    let row = CONTROL_SIZE;
    // The session menu or a slider takes the whole row while it's open.
    let c = &view.controls;
    let slider = c.slider.and_then(|s| match s {
        Slider::Volume => c.volume.map(|l| (s, l)),
        Slider::Mic => c.mic.map(|l| (s, l)),
        Slider::Brightness => c.brightness.map(|l| (s, l)),
    });
    let taken = c.session_open || slider.is_some();
    let controls_end = if c.session_open {
        session_row(c, y, w, theme, m, &mut draws, &mut hits);
        w
    } else if let Some(open) = slider {
        slider_row(open, y, w, theme, m, &mut draws, &mut hits);
        w
    } else {
        controls_row(c, y, theme, &mut draws, &mut hits)
    };
    let tray: &[TrayIconView] = if taken { &[] } else { tray };
    // The power button sits at the far right, out of the way of the controls; its hit
    // goes in after the tray's so keyboard order follows the row.
    let mut power_hit = Vec::new();
    let mut right_edge = w - 14.0;
    if !taken {
        let rect = Rect::new(w - 10.0 - row, y, row, row);
        let target = Target::SessionMenu(true);
        round_button("power", rect, target, theme, &mut draws, &mut power_hit);
        right_edge = rect.x - 8.0;
    }
    if let Some(battery) = view.battery.as_ref().filter(|_| !taken) {
        // As much as fits between the controls and the power button: "76% · Charging",
        // "76%", or the icon alone (it shows charging and the level too).
        let size = 22.0;
        let room = right_edge - (controls_end + 8.0) - size;
        let style = TextStyle::new(12.5, Weight::Semibold);
        let percent = format!("{}%", battery.percent);
        let mut labels = vec![percent.clone()];
        if battery.charging {
            labels.insert(0, format!("{percent} · Charging"));
        }
        let label = labels.into_iter().find_map(|label| {
            let (t, tw, th) = text(m, &label, style, theme.text, 160.0, 1);
            (tw + 3.0 <= room).then_some((t, tw, th))
        });
        let mut x = right_edge;
        if let Some((t, tw, th)) = label {
            x -= tw;
            draws.push(place(t, x, y + (row - th) / 2.0));
            x -= 3.0;
        }
        let icon_rect = Rect::new(x - size, y + (row - size) / 2.0, size, size);
        draws.extend(battery_icon(battery, theme, icon_rect));
        right_edge = icon_rect.x - 6.0;
    }
    let slot = 34.0;
    let inline_x = controls_end + 10.0;
    let inline = inline_x + tray.len() as f32 * slot <= right_edge;
    let mut tray_hits = Vec::new();
    let (tray_x, tray_y) = if inline {
        (inline_x, y + (row - slot) / 2.0)
    } else {
        (8.0, y + row + 6.0)
    };
    for (i, item) in tray.iter().enumerate() {
        let rect = Rect::new(tray_x + i as f32 * slot, tray_y, slot, slot);
        draws.push(tray_icon(item, theme, rect, 20.0));
        tray_hits.push(Hit {
            rect,
            target: Target::Tray {
                id: item.id.clone(),
            },
            highlight: true,
            keyboard: true,
        });
    }
    // Keyboard order follows the rows.
    if inline {
        hits.append(&mut tray_hits);
        hits.append(&mut power_hit);
    } else {
        hits.append(&mut power_hit);
        hits.append(&mut tray_hits);
    }
    y += row + 10.0;
    if !inline && !tray.is_empty() {
        y += slot + 6.0;
    }
    if !items.is_empty() {
        draws.push(Draw::Rect(RoundRect {
            rect: Rect::new(14.0, y, w - 28.0, 1.0),
            radius: 0.0,
            color: theme.button,
        }));
        y += 5.0;
    }
    // A window of LIST_ROWS rows, scrolled by `right_scroll` (wheel, or the rows below).
    let offset = view.right_scroll.min(items.len().saturating_sub(LIST_ROWS));
    if offset > 0 {
        let label = format!("{offset} newer");
        let delta = Some(-(LIST_ROWS as i32));
        scroll_row(
            &label,
            "chevron-up",
            delta,
            y,
            w,
            theme,
            m,
            &mut draws,
            &mut hits,
        );
        y += 28.0;
    }
    let visible = &items[offset..(offset + LIST_ROWS).min(items.len())];
    let below = total - offset - visible.len();
    let more_row = (below > 0).then(|| {
        // Rows past what the view carries can't be scrolled to; they're only counted.
        let scrollable = offset + visible.len() < items.len();
        (
            format!("{below} more"),
            scrollable.then_some(LIST_ROWS as i32),
        )
    });
    for a in visible {
        let row = Rect::new(6.0, y, w - 12.0, ROW_HEIGHT);
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
        // Close button first so it wins over the row.
        let close_rect = Rect::new(
            w - 8.0 - close,
            y + (ROW_HEIGHT - close) / 2.0,
            close,
            close,
        );
        draws.push(Draw::Icon(IconDraw {
            name: "x".into(),
            rect: Rect::new(close_rect.x + 8.0, close_rect.y + 8.0, 12.0, 12.0),
            color: theme.text_secondary,
        }));
        hits.push(Hit {
            rect: close_rect,
            target: Target::Dismiss { id: a.id.clone() },
            highlight: true,
            keyboard: false,
        });
        // Clicking a row opens it (its default action) or, without one, dismisses it.
        let target = if a.has_action(DEFAULT_ACTION) {
            Target::Invoke {
                id: a.id.clone(),
                key: DEFAULT_ACTION.into(),
            }
        } else {
            Target::Dismiss { id: a.id.clone() }
        };
        hits.push(Hit {
            rect: row,
            target,
            highlight: true,
            keyboard: true,
        });
        y += ROW_HEIGHT;
    }
    if let Some((label, delta)) = more_row {
        scroll_row(
            &label,
            "chevron-down",
            delta,
            y,
            w,
            theme,
            m,
            &mut draws,
            &mut hits,
        );
        y += 28.0;
    }
    if total > 1 {
        let button = Rect::new(6.0, y + 2.0, w - 12.0, 30.0);
        let style = TextStyle::new(12.5, Weight::Semibold);
        let (t, tw, th) = text(m, "Clear all", style, theme.text_secondary, text_w, 1);
        draws.push(place(t, (w - tw) / 2.0, button.y + (button.h - th) / 2.0));
        hits.push(Hit {
            rect: button,
            target: Target::ClearRight,
            highlight: true,
            keyboard: true,
        });
        y += 34.0;
    }
    Content {
        w,
        h: y + 8.0,
        items: draws,
        hits,
        key: if taken { "r:list:taken" } else { "r:list" }.into(),
    }
}

/// A "N newer" / "N more" row in the expanded list; scrolls by `delta` rows when clicked
/// (or just counts, with `None`).
#[allow(clippy::too_many_arguments)]
fn scroll_row(
    label: &str,
    icon_name: &str,
    delta: Option<i32>,
    y: f32,
    w: f32,
    theme: &Theme,
    m: &mut dyn Measure,
    draws: &mut Vec<Draw>,
    hits: &mut Vec<Hit>,
) {
    let h = 28.0;
    let style = TextStyle::new(12.0, Weight::Medium);
    let (t, tw, th) = text(m, label, style, theme.text_secondary, w - 40.0, 1);
    let size = 12.0;
    let x = (w - tw - size - 6.0) / 2.0;
    draws.push(Draw::Icon(IconDraw {
        name: icon_name.into(),
        rect: Rect::new(x, y + (h - size) / 2.0, size, size),
        color: theme.text_secondary,
    }));
    draws.push(place(t, x + size + 6.0, y + (h - th) / 2.0));
    if let Some(delta) = delta {
        hits.push(Hit {
            rect: Rect::new(6.0, y, w - 12.0, h),
            target: Target::Scroll(delta),
            highlight: true,
            keyboard: true,
        });
    }
}

/// Clicking anywhere on an activity with a `default` action invokes it.
fn default_hit(a: &ActivityView, w: f32, h: f32) -> Option<Hit> {
    a.has_action(DEFAULT_ACTION).then(|| Hit {
        rect: Rect::new(0.0, 0.0, w, h),
        target: Target::Invoke {
            id: a.id.clone(),
            key: DEFAULT_ACTION.into(),
        },
        highlight: false,
        keyboard: true,
    })
}

/// Lays out action buttons at `y`; returns the row height. Icon-only actions (media
/// controls) become a centered row of icon buttons, others share the width equally.
#[allow(clippy::too_many_arguments)]
fn action_row(
    a: &ActivityView,
    buttons: &[&Action],
    theme: &Theme,
    m: &mut dyn Measure,
    x: f32,
    y: f32,
    width: f32,
    items: &mut Vec<Draw>,
    hits: &mut Vec<Hit>,
) -> f32 {
    let invoke = |b: &Action| Target::Invoke {
        id: a.id.clone(),
        key: b.key.clone(),
    };
    let icon_only = buttons
        .iter()
        .all(|b| b.icon.is_some() && b.label.is_empty());
    if icon_only {
        let (bw, bh, gap, size) = (48.0, 36.0, 14.0, 22.0);
        let total = buttons.len() as f32 * bw + (buttons.len() - 1) as f32 * gap;
        let mut bx = x + (width - total) / 2.0;
        for b in buttons {
            let rect = Rect::new(bx, y, bw, bh);
            items.push(Draw::Icon(IconDraw {
                name: b.icon.clone().unwrap_or_default(),
                rect: Rect::new(bx + (bw - size) / 2.0, y + (bh - size) / 2.0, size, size),
                color: theme.text,
            }));
            hits.push(Hit {
                rect,
                target: invoke(b),
                highlight: true,
                keyboard: true,
            });
            bx += bw + gap;
        }
        return bh;
    }
    let (bh, gap) = (32.0, 8.0);
    let bw = (width - gap * (buttons.len() - 1) as f32) / buttons.len() as f32;
    for (i, b) in buttons.iter().enumerate() {
        let rect = Rect::new(x + i as f32 * (bw + gap), y, bw, bh);
        items.push(Draw::Rect(RoundRect {
            rect,
            radius: 10.0,
            color: theme.button,
        }));
        let style = TextStyle::new(13.0, Weight::Semibold);
        let label = if b.label.is_empty() { &b.key } else { &b.label };
        let (t, tw, th) = text(m, label, style, theme.text, bw - 16.0, 1);
        items.push(place(t, rect.x + (bw - tw) / 2.0, y + (bh - th) / 2.0));
        hits.push(Hit {
            rect,
            target: invoke(b),
            highlight: true,
            keyboard: true,
        });
    }
    bh
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
            image: None,
            app: None,
            actions: Vec::new(),
            priority: Priority::Normal,
            lasting: false,
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
            ..View::default()
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
        // DND, keep awake, power, close button, row, close button, row, clear all.
        let targets: Vec<_> = list.hits.iter().map(|h| h.target.clone()).collect();
        assert_eq!(targets[0], Target::Control(Control::Dnd));
        assert_eq!(targets[1], Target::Control(Control::KeepAwake));
        assert_eq!(targets[2], Target::SessionMenu(true));
        let power = list.hits[2].rect;
        assert_eq!(
            power.right(),
            theme.right_expanded_width - 10.0,
            "far right"
        );
        assert_eq!(targets[3], Target::Dismiss { id: "a".into() });
        assert_eq!(targets.last(), Some(&Target::ClearRight));
        assert_eq!(targets.len(), 8);
        assert!(right_content(&view(None, &[]), false, &theme, &mut m).is_none());
        // Expanded still offers the quick controls.
        assert!(right_content(&view(None, &[]), true, &theme, &mut m).is_some());
    }

    #[test]
    fn expanded_actions_become_buttons_and_default_is_last() {
        let theme = Theme::default();
        let a = ActivityView {
            actions: vec![
                Action::new("default", ""),
                Action::new("allow", "Allow"),
                Action::new("deny", "Deny"),
            ],
            ..activity("perm", None)
        };
        let without = center_content(
            &view(Some(activity("perm", None)), &[]),
            true,
            &theme,
            &mut FakeMeasure,
        );
        let c = center_content(&view(Some(a), &[]), true, &theme, &mut FakeMeasure);
        assert!(c.h > without.h, "button row adds height");
        let keys: Vec<_> = c
            .hits
            .iter()
            .map(|h| match &h.target {
                Target::Invoke { key, .. } => key.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(keys, ["allow", "deny", "default"]);
        assert!(!c.hits[2].highlight);
    }

    #[test]
    fn icon_only_actions_are_centered() {
        let theme = Theme::default();
        let icon_action = |key: &str, icon: &str| Action {
            icon: Some(icon.into()),
            ..Action::new(key, "")
        };
        let a = ActivityView {
            actions: vec![
                icon_action("prev", "skip-back"),
                icon_action("next", "skip-forward"),
            ],
            ..activity("media", None)
        };
        let c = center_content(&view(Some(a), &[]), true, &theme, &mut FakeMeasure);
        let (l, r) = (c.hits[0].rect, c.hits[1].rect);
        let mid = (l.x + r.right()) / 2.0;
        assert!((mid - c.w / 2.0).abs() < 0.01);
    }

    fn tray(n: usize) -> Vec<TrayIconView> {
        (0..n)
            .map(|i| TrayIconView {
                id: format!(":1.{i}/StatusNotifierItem"),
                title: format!("app {i}"),
                image: Some(format!("img{i}")),
                needs_attention: false,
            })
            .collect()
    }

    #[test]
    fn tray_alone_shows_the_right_island_and_widens_it() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let mut v = view(None, &[]);
        assert!(right_content(&v, false, &theme, &mut m).is_none());
        v.tray = tray(2);
        let only_tray = right_content(&v, false, &theme, &mut m).unwrap();
        assert_eq!(only_tray.hits.len(), 2);
        let with_item =
            right_content(&view(None, &[activity("a", None)]), false, &theme, &mut m).unwrap();
        v.right = view(None, &[activity("a", None)]).right;
        let both = right_content(&v, false, &theme, &mut m).unwrap();
        assert!(both.w > with_item.w && both.w > only_tray.w);
        // Expanded: controls, then the tray icons, then the list.
        let list = right_content(&v, true, &theme, &mut m).unwrap();
        assert!(matches!(list.hits[0].target, Target::Control(_)));
        let tray = list
            .hits
            .iter()
            .find(|h| matches!(h.target, Target::Tray { .. }))
            .expect("tray icon");
        // Few icons share the header row with the controls.
        assert_eq!(list.hits[0].rect.y, 10.0);
        assert!(tray.rect.y < 10.0 + CONTROL_SIZE);
    }

    #[test]
    fn open_menu_replaces_the_right_island() {
        use crate::activity::MenuEntryView;
        let theme = Theme::default();
        let entry = |id, label: &str| MenuEntryView {
            id,
            label: label.into(),
            enabled: id != 3,
            separator: false,
            toggle: None,
            submenu: id == 2,
        };
        let mut v = view(None, &[activity("a", None)]);
        v.menu = Some(MenuView {
            title: "app".into(),
            entries: vec![entry(1, "Open"), entry(2, "More"), entry(3, "Disabled")],
            nested: true,
            offset: 0,
        });
        let c = right_content(&v, false, &theme, &mut FakeMeasure).unwrap();
        let targets: Vec<_> = c.hits.iter().map(|h| h.target.clone()).collect();
        assert_eq!(
            targets,
            [
                Target::MenuBack,
                Target::MenuItem { id: 1 },
                Target::MenuItem { id: 2 }
            ]
        );
    }

    #[test]
    fn battery_is_always_on_the_right_and_fills_by_charge() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let mut v = view(None, &[]);
        v.battery = Some(BatteryView {
            percent: 50,
            charging: false,
            level: BatteryLevel::Normal,
        });
        let c = right_content(&v, false, &theme, &mut m).expect("battery shows the island");
        let fill = c
            .items
            .iter()
            .find_map(|d| match d {
                Draw::Rect(r) if r.color == theme.text => Some(r.rect),
                _ => None,
            })
            .expect("fill");
        let full_width = 11.0 * 20.0 / 24.0;
        assert!((fill.w - full_width / 2.0).abs() < 0.01);

        v.battery = Some(BatteryView {
            percent: 4,
            charging: false,
            level: BatteryLevel::Critical,
        });
        let c = right_content(&v, false, &theme, &mut m).unwrap();
        let red = theme.priority_color(Priority::Critical);
        assert!(
            c.items
                .iter()
                .any(|d| matches!(d, Draw::Rect(r) if r.color == red))
        );
    }

    #[test]
    fn controls_show_wifi_only_with_a_device() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let mut v = view(None, &[]);
        let controls = |c: &Content| -> Vec<Target> {
            c.hits
                .iter()
                .filter(|h| matches!(h.target, Target::Control(_)))
                .map(|h| h.target.clone())
                .collect()
        };
        let without = right_content(&v, true, &theme, &mut m).unwrap();
        let dnd = Target::Control(Control::Dnd);
        let awake = Target::Control(Control::KeepAwake);
        // (The power button opens a menu; it isn't a control.)
        assert_eq!(controls(&without), [dnd.clone(), awake.clone()]);
        v.controls.wifi = Some(true);
        let with = right_content(&v, true, &theme, &mut m).unwrap();
        assert_eq!(
            controls(&with),
            [Target::Control(Control::Wifi), dnd, awake]
        );
    }

    #[test]
    fn session_menu_replaces_the_controls_row() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let mut v = view(None, &[activity("a", None)]);
        v.battery = Some(BatteryView {
            percent: 50,
            charging: false,
            level: BatteryLevel::Normal,
        });
        v.controls.session_open = true;
        let open = right_content(&v, true, &theme, &mut m).unwrap();
        let targets: Vec<_> = open.hits.iter().map(|h| h.target.clone()).collect();
        assert_eq!(
            targets[..4],
            [
                Target::SessionMenu(false),
                Target::Session(SessionAction::Poweroff),
                Target::Session(SessionAction::Reboot),
                Target::Session(SessionAction::Logout),
            ]
        );
        assert!(!targets.iter().any(|t| matches!(t, Target::Control(_))));
        // The options share the row up to the island's padding.
        let last = &open.hits[3].rect;
        assert!((last.right() - (theme.right_expanded_width - 10.0)).abs() < 0.5);
        let closed_key = {
            v.controls.session_open = false;
            right_content(&v, true, &theme, &mut m).unwrap().key
        };
        assert_ne!(open.key, closed_key, "opening cross-fades");

        // Holding fills the option in the critical color; the key doesn't change.
        v.controls.session_open = true;
        v.controls.session_hold = Some((SessionAction::Reboot, 0.5));
        let held = right_content(&v, true, &theme, &mut m).unwrap();
        assert_eq!(held.key, open.key);
        let red = theme.priority_color(Priority::Critical);
        let fill = held
            .items
            .iter()
            .find_map(|d| match d {
                Draw::Rect(r) if r.color.r == red.r && r.color.g == red.g => Some(r.rect),
                _ => None,
            })
            .expect("fill");
        let reboot = held.hits[2].rect;
        assert_eq!(fill.x, reboot.x);
        assert!(fill.w > CONTROL_SIZE && fill.w < reboot.w);
    }

    #[test]
    fn volume_and_brightness_buttons_and_sliders() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let mut v = view(None, &[activity("a", None)]);
        let targets = |c: &Content| -> Vec<Target> {
            c.hits
                .iter()
                .filter(|h| {
                    matches!(
                        h.target,
                        Target::Level(_) | Target::Slider(_) | Target::CloseSlider
                    )
                })
                .map(|h| h.target.clone())
                .collect()
        };
        let without = right_content(&v, true, &theme, &mut m).unwrap();
        assert!(targets(&without).is_empty());

        v.controls.volume = Some(Level {
            value: 0.45,
            muted: false,
        });
        v.controls.brightness = Some(Level {
            value: 0.7,
            muted: false,
        });
        let buttons = right_content(&v, true, &theme, &mut m).unwrap();
        assert_eq!(
            targets(&buttons),
            [
                Target::Level(Slider::Volume),
                Target::Level(Slider::Brightness)
            ]
        );
        // Round buttons in the controls row, after keep awake: no extra row, no text.
        assert_eq!(buttons.h, without.h);
        let hit = |c: &Content, t: &Target| c.hits.iter().find(|h| h.target == *t).unwrap().rect;
        let awake = hit(&buttons, &Target::Control(Control::KeepAwake));
        let volume = hit(&buttons, &Target::Level(Slider::Volume));
        assert_eq!((volume.y, volume.w, volume.h), (awake.y, awake.w, awake.h));
        assert!(volume.x > awake.x);
        let has_text = |c: &Content, s: &str| {
            c.items
                .iter()
                .any(|d| matches!(d, Draw::Text(t) if t.text == s))
        };
        assert!(!has_text(&buttons, "45%"));
        let icons = |c: &Content| -> Vec<String> {
            c.items
                .iter()
                .filter_map(|d| match d {
                    Draw::Icon(i) => Some(i.name.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(icons(&buttons).contains(&"volume".to_owned()));
        v.controls.volume.as_mut().unwrap().muted = true;
        let muted = right_content(&v, true, &theme, &mut m).unwrap();
        assert!(icons(&muted).contains(&"volume-mute".to_owned()));

        // The slider takes the whole controls row, like the session menu.
        v.controls.slider = Some(Slider::Volume);
        let slider = right_content(&v, true, &theme, &mut m).unwrap();
        assert_eq!(
            targets(&slider),
            [Target::CloseSlider, Target::Slider(Slider::Volume)]
        );
        assert!(has_text(&slider, "Muted"));
        assert!(
            !slider
                .hits
                .iter()
                .any(|h| matches!(h.target, Target::Control(_) | Target::SessionMenu(_)))
        );
        assert_eq!(slider.h, buttons.h, "same height, no jump");
        assert_ne!(slider.key, buttons.key, "opening cross-fades");

        // A slider for a level that went away falls back to the buttons.
        v.controls.slider = Some(Slider::Brightness);
        v.controls.brightness = None;
        let gone = right_content(&v, true, &theme, &mut m).unwrap();
        assert_eq!(targets(&gone), [Target::Level(Slider::Volume)]);

        // The microphone sits between volume and brightness, crossed out when muted.
        v.controls.slider = None;
        v.controls.brightness = Some(Level {
            value: 0.7,
            muted: false,
        });
        v.controls.mic = Some(Level {
            value: 0.6,
            muted: true,
        });
        let with_mic = right_content(&v, true, &theme, &mut m).unwrap();
        assert_eq!(
            targets(&with_mic),
            [
                Target::Level(Slider::Volume),
                Target::Level(Slider::Mic),
                Target::Level(Slider::Brightness)
            ]
        );
        assert!(icons(&with_mic).contains(&"mic-off".to_owned()));
        v.controls.slider = Some(Slider::Mic);
        let mic_slider = right_content(&v, true, &theme, &mut m).unwrap();
        assert_eq!(
            targets(&mic_slider),
            [Target::CloseSlider, Target::Slider(Slider::Mic)]
        );
        // The label never runs into the track.
        let track = mic_slider.hits[1].rect;
        let label_x = mic_slider
            .items
            .iter()
            .find_map(|d| match d {
                Draw::Text(t) if t.text == "Muted" => Some(t.rect.x),
                _ => None,
            })
            .unwrap();
        assert!(label_x >= track.right() + 8.0);
    }

    #[test]
    fn battery_label_shortens_when_the_row_is_full() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let mut v = view(None, &[]);
        v.battery = Some(BatteryView {
            percent: 76,
            charging: true,
            level: BatteryLevel::Normal,
        });
        let label = |v: &View, m: &mut FakeMeasure| {
            right_content(v, true, &theme, m)
                .unwrap()
                .items
                .iter()
                .find_map(|d| match d {
                    Draw::Text(t) if t.text.contains('%') => Some(t.text.clone()),
                    _ => None,
                })
        };
        assert_eq!(label(&v, &mut m).as_deref(), Some("76% · Charging"));
        v.controls.wifi = Some(true);
        let level = Some(Level {
            value: 0.5,
            muted: false,
        });
        v.controls.volume = level;
        v.controls.brightness = level;
        let short = label(&v, &mut m);
        assert!(
            short.is_none() || short.as_deref() == Some("76%"),
            "{short:?}"
        );
    }

    #[test]
    fn clock_shows_beside_lasting_center_activities() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let has_clock = |c: &Content| {
            c.items
                .iter()
                .any(|d| matches!(d, Draw::Text(t) if t.text == "13:35"))
        };
        let brief = view(Some(activity("osd", Some(0.4))), &[]);
        let media = view(
            Some(ActivityView {
                lasting: true,
                ..activity("media", None)
            }),
            &[],
        );
        for expanded in [false, true] {
            assert!(!has_clock(&center_content(
                &brief, expanded, &theme, &mut m
            )));
            assert!(has_clock(&center_content(&media, expanded, &theme, &mut m)));
        }
        // The clock makes the pill wider, not the title shorter.
        let without = ActivityView {
            lasting: false,
            ..media.center.clone().unwrap()
        };
        let narrow = center_content(&view(Some(without), &[]), false, &theme, &mut m);
        let wide = center_content(&media, false, &theme, &mut m);
        assert!(wide.w > narrow.w);
    }

    #[test]
    fn privacy_icons_lead_the_right_island() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let mut v = view(None, &[]);
        assert!(right_content(&v, false, &theme, &mut m).is_none());
        v.privacy.mic = true;
        v.privacy.camera = true;
        let c = right_content(&v, false, &theme, &mut m).expect("shown for the icons alone");
        let icons: Vec<_> = c
            .items
            .iter()
            .filter_map(|d| match d {
                Draw::Icon(i) => Some((i.name.as_str(), i.color)),
                _ => None,
            })
            .collect();
        assert_eq!(
            icons,
            [
                ("mic", theme.priority_color(Priority::High)),
                ("camera", theme.charging)
            ]
        );
        // Ends right after the last icon, like the left padding.
        assert!(c.w < 10.0 + 2.0 * 14.0 + 6.0 + PAD + 0.5);

        v.right = Some(RightView {
            items: vec![activity("a", None)],
            total: 1,
        });
        let with_item = right_content(&v, false, &theme, &mut m).unwrap();
        let first_icon = with_item.items.iter().find_map(|d| match d {
            Draw::Icon(i) => Some(i.name.clone()),
            _ => None,
        });
        assert_eq!(first_icon.as_deref(), Some("mic"), "before the newest item");

        // The announcement of a capture doesn't repeat the indicator's icon.
        v.right = Some(RightView {
            items: vec![ActivityView {
                icon: Some("mic".into()),
                ..activity("privacy:mic", None)
            }],
            total: 1,
        });
        let announced = right_content(&v, false, &theme, &mut m).unwrap();
        let mics = announced
            .items
            .iter()
            .filter(|d| matches!(d, Draw::Icon(i) if i.name == "mic"))
            .count();
        assert_eq!(mics, 1);
    }

    #[test]
    fn tray_menu_is_as_wide_as_the_expanded_island() {
        let mut m = FakeMeasure;
        let mut v = view(None, &[]);
        v.menu = Some(MenuView {
            title: "App".into(),
            entries: vec![],
            nested: false,
            offset: 0,
        });
        for width in [340.0, 420.0] {
            let theme = Theme {
                right_expanded_width: width,
                ..Theme::default()
            };
            let menu = right_content(&v, true, &theme, &mut m).unwrap();
            assert_eq!(menu.w, width);
        }
    }

    #[test]
    fn temperature_sits_right_before_the_battery() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let mut v = view(None, &[]);
        v.weather = Some("11°".into());
        let alone = right_content(&v, false, &theme, &mut m).expect("shown on its own");
        assert!(
            alone
                .items
                .iter()
                .any(|d| matches!(d, Draw::Text(t) if t.text == "11°"))
        );
        v.battery = Some(BatteryView {
            percent: 76,
            charging: false,
            level: BatteryLevel::Normal,
        });
        v.controls.dnd = true;
        let c = right_content(&v, false, &theme, &mut m).unwrap();
        let x_of = |name: &str| -> f32 {
            c.items
                .iter()
                .find_map(|d| match d {
                    Draw::Text(t) if t.text == name => Some(t.rect.x),
                    Draw::Icon(i) if i.name == name => Some(i.rect.x),
                    _ => None,
                })
                .unwrap()
        };
        // The battery icon comes just before its percent.
        let battery_x = x_of("76%") - 24.0;
        assert!(x_of("moon") < x_of("11°"));
        assert!(x_of("11°") < battery_x);
        v.weather = None;
        let without = right_content(&v, false, &theme, &mut m).unwrap();
        assert!(without.w < c.w);
    }

    #[test]
    fn slider_value_maps_the_track() {
        let track = Rect::new(100.0, 0.0, 238.0, 38.0);
        // The icon circle at the left is 0, the right end's circle center is 1.
        assert_eq!(slider_value(track, 100.0), 0.0);
        assert_eq!(slider_value(track, 119.0), 0.0);
        assert_eq!(slider_value(track, 219.0), 0.5);
        assert_eq!(slider_value(track, 319.0), 1.0);
        assert_eq!(slider_value(track, 500.0), 1.0);
    }

    #[test]
    fn list_scrolls_in_windows_of_rows() {
        let theme = Theme::default();
        let mut m = FakeMeasure;
        let items: Vec<_> = (0..10).map(|i| activity(&format!("n{i}"), None)).collect();
        let mut v = view(None, &items);
        let rows = |c: &Content| -> Vec<String> {
            c.hits
                .iter()
                .filter_map(|h| match &h.target {
                    Target::Dismiss { id } if h.keyboard => Some(id.clone()),
                    _ => None,
                })
                .collect()
        };
        let scrolls = |c: &Content| -> Vec<i32> {
            c.hits
                .iter()
                .filter_map(|h| match h.target {
                    Target::Scroll(d) => Some(d),
                    _ => None,
                })
                .collect()
        };
        let top = right_content(&v, true, &theme, &mut m).unwrap();
        assert_eq!(rows(&top), ["n0", "n1", "n2", "n3", "n4", "n5"]);
        assert_eq!(scrolls(&top), [LIST_ROWS as i32], "only '4 more'");

        v.right_scroll = 6;
        let down = right_content(&v, true, &theme, &mut m).unwrap();
        assert_eq!(
            rows(&down),
            ["n4", "n5", "n6", "n7", "n8", "n9"],
            "clamped to the end"
        );
        assert_eq!(scrolls(&down), [-(LIST_ROWS as i32)], "only '4 newer'");
    }

    #[test]
    fn images_replace_icons_and_app_is_a_caption() {
        let theme = Theme::default();
        let a = ActivityView {
            image: Some("file:/tmp/avatar.png".into()),
            app: Some("Telegram".into()),
            ..activity("n", None)
        };
        let c = center_content(&view(Some(a), &[]), true, &theme, &mut FakeMeasure);
        assert!(
            c.items
                .iter()
                .any(|d| matches!(d, Draw::Image(i) if i.key == "file:/tmp/avatar.png"))
        );
        assert!(!c.items.iter().any(|d| matches!(d, Draw::Icon(_))));
        let caption = c
            .items
            .iter()
            .any(|d| matches!(d, Draw::Text(t) if t.text == "Telegram"));
        assert!(caption);
    }

    #[test]
    fn long_menus_scroll() {
        use crate::activity::MenuEntryView;
        let theme = Theme::default();
        let entries: Vec<_> = (0..20)
            .map(|id| MenuEntryView {
                id,
                label: format!("Entry {id}"),
                enabled: true,
                separator: false,
                toggle: None,
                submenu: false,
            })
            .collect();
        let mut v = view(None, &[]);
        v.menu = Some(MenuView {
            title: "app".into(),
            entries,
            nested: false,
            offset: 0,
        });
        let items = |c: &Content| -> Vec<i32> {
            c.hits
                .iter()
                .filter_map(|h| match h.target {
                    Target::MenuItem { id } => Some(id),
                    _ => None,
                })
                .collect()
        };
        let top = right_content(&v, false, &theme, &mut FakeMeasure).unwrap();
        assert_eq!(items(&top), (0..12).collect::<Vec<_>>());
        assert!(
            top.hits
                .iter()
                .any(|h| h.target == Target::Scroll(MENU_ROWS as i32))
        );

        v.menu.as_mut().unwrap().offset = 100;
        let end = right_content(&v, false, &theme, &mut FakeMeasure).unwrap();
        assert_eq!(
            items(&end),
            (8..20).collect::<Vec<_>>(),
            "clamped to the end"
        );
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
