//! Spring animations and the per-output [`Stage`] that morphs both islands toward what the
//! view asks for.
//!
//! - Island geometry follows slightly underdamped springs (a little Apple-like overshoot).
//! - When an island shows a different thing, its content cross-fades in.
//! - When a `flow` activity leaves the center for the right island, it travels there: the
//!   right island flies out of the center if it was hidden, otherwise a "ghost" pill
//!   carries the content across and merges into it.
//!
//! Everything here is pure: callers pass `dt`, so tests and the headless renderer can
//! simulate animations frame by frame.

use std::f32::consts::TAU;

use crate::activity::View;
use crate::layout::{
    Content, Draw, Group, Hit, Measure, Motion, Rect, RoundRect, Scene, Target, Theme,
    center_content, right_content,
};

/// Spring constants derived from a response time (seconds per oscillation) and a damping
/// ratio (1.0 = no overshoot).
#[derive(Clone, Copy, Debug)]
pub struct Params {
    stiffness: f32,
    damping: f32,
    /// Below this distance (and a matching speed) the spring snaps to its target.
    epsilon: f32,
}

impl Params {
    pub fn new(response: f32, damping_ratio: f32, epsilon: f32) -> Self {
        let omega = TAU / response;
        Self {
            stiffness: omega * omega,
            damping: 2.0 * damping_ratio * omega,
            epsilon,
        }
    }
}

fn morph(m: Motion) -> Params {
    Params::new(m.response, m.damping, 0.1)
}

fn fade() -> Params {
    Params::new(0.22, 1.0, 0.004)
}

fn ghost_fade() -> Params {
    Params::new(0.32, 1.0, 0.004)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
}

impl Spring {
    pub fn new(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
        }
    }

    pub fn snap(&mut self, value: f32) {
        *self = Self::new(value);
    }

    pub fn settled(&self) -> bool {
        self.value == self.target && self.velocity == 0.0
    }

    pub fn step(&mut self, dt: f32, p: Params) {
        if self.settled() {
            return;
        }
        // Semi-implicit Euler in small substeps stays stable for any frame time.
        let mut left = dt;
        while left > 0.0 {
            let h = left.min(1.0 / 240.0);
            let force = -p.stiffness * (self.value - self.target) - p.damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
            left -= h;
        }
        if (self.value - self.target).abs() < p.epsilon && self.velocity.abs() < p.epsilon * 10.0 {
            self.snap(self.target);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RectSpring {
    x: Spring,
    y: Spring,
    w: Spring,
    h: Spring,
}

impl RectSpring {
    fn new(r: Rect) -> Self {
        Self {
            x: Spring::new(r.x),
            y: Spring::new(r.y),
            w: Spring::new(r.w),
            h: Spring::new(r.h),
        }
    }

    fn springs(&mut self) -> [&mut Spring; 4] {
        [&mut self.x, &mut self.y, &mut self.w, &mut self.h]
    }

    fn snap(&mut self, r: Rect) {
        *self = Self::new(r);
    }

    fn set_target(&mut self, r: Rect) {
        self.x.target = r.x;
        self.y.target = r.y;
        self.w.target = r.w;
        self.h.target = r.h;
    }

    fn current(&self) -> Rect {
        Rect::new(
            self.x.value,
            self.y.value,
            self.w.value.max(0.0),
            self.h.value.max(0.0),
        )
    }

    fn target(&self) -> Rect {
        Rect::new(self.x.target, self.y.target, self.w.target, self.h.target)
    }

    fn step(&mut self, dt: f32, motion: Motion) {
        self.springs()
            .into_iter()
            .for_each(|s| s.step(dt, morph(motion)));
    }

    fn finish(&mut self) {
        self.springs().into_iter().for_each(|s| s.snap(s.target));
    }

    fn settled(&self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(Spring::settled)
    }
}

#[derive(Clone, Debug)]
struct Island {
    rect: RectSpring,
    content: Option<Content>,
    /// Content opacity; geometry is separate so content can fade while the pill morphs.
    alpha: Spring,
}

impl Island {
    fn new(rect: Rect, content: Option<Content>) -> Self {
        let alpha = Spring::new(if content.is_some() { 1.0 } else { 0.0 });
        Self {
            rect: RectSpring::new(rect),
            content,
            alpha,
        }
    }

    fn set(&mut self, target: Rect, content: Option<Content>) {
        self.rect.set_target(target);
        match content {
            Some(c) => {
                if self.content.as_ref().map(|o| &o.key) != Some(&c.key) {
                    self.alpha.snap(0.0);
                }
                self.alpha.target = 1.0;
                self.content = Some(c);
            }
            // Keep the old content so it can fade out.
            None => self.alpha.target = 0.0,
        }
    }

    /// Jumps to the end of every animation (motion turned off).
    fn finish(&mut self) {
        self.rect.finish();
        self.alpha.snap(self.alpha.target);
        if self.alpha.value == 0.0 {
            self.content = None;
        }
    }

    fn step(&mut self, dt: f32, motion: Motion) {
        self.rect.step(dt, motion);
        self.alpha.step(dt, fade());
        if self.alpha.settled() && self.alpha.value == 0.0 {
            self.content = None;
        }
    }

    fn settled(&self) -> bool {
        self.rect.settled() && self.alpha.settled()
    }
}

/// Carries content from the center to the right island.
#[derive(Clone, Debug)]
struct Ghost {
    rect: RectSpring,
    content: Content,
    alpha: Spring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandKind {
    Center,
    Right,
}

/// Animated state of both islands on one output.
#[derive(Clone, Debug, Default)]
pub struct Stage {
    center: Option<Island>,
    right: Option<Island>,
    ghost: Option<Ghost>,
    width: f32,
    /// Id of the activity the center showed after the last update.
    center_id: Option<String>,
    /// Pointer position in surface coordinates, for hover highlights.
    pointer: Option<(f32, f32)>,
    motion: Motion,
    /// The island being navigated with the keyboard, and the selected target in it.
    keyboard: Option<IslandKind>,
    selection: usize,
}

impl Stage {
    /// Points every island at what `view` asks for. The first update (and any width
    /// change) snaps instead of animating.
    pub fn update(
        &mut self,
        width: f32,
        view: &View,
        hover: Option<IslandKind>,
        theme: &Theme,
        m: &mut dyn Measure,
    ) {
        let center_c = center_content(view, hover == Some(IslandKind::Center), theme, m);
        let right_c = right_content(view, hover == Some(IslandKind::Right), theme, m);
        let center_target = Rect::new(
            (width - center_c.w) / 2.0,
            theme.pill_top,
            center_c.w,
            center_c.h,
        );
        let right_target = match &right_c {
            Some(c) => Rect::new(width - theme.right_margin - c.w, theme.pill_top, c.w, c.h),
            None => hidden_right(width, theme),
        };
        let center_id = view.center.as_ref().map(|a| a.id.clone());
        self.motion = theme.motion;

        let (Some(center), Some(right)) = (&mut self.center, &mut self.right) else {
            return self.snap(
                width,
                center_target,
                center_c,
                right_target,
                right_c,
                center_id,
            );
        };
        if width != self.width {
            return self.snap(
                width,
                center_target,
                center_c,
                right_target,
                right_c,
                center_id,
            );
        }

        let moved_right = self.center_id.as_ref().is_some_and(|old| {
            center_id.as_ref() != Some(old)
                && view.right.as_ref().is_some_and(|r| &r.newest().id == old)
        });
        if moved_right {
            let from = center.rect.current();
            if right.content.is_none() || right.rect.current().w < 1.0 {
                // The right island flies out of the center carrying its new content.
                right.rect.snap(from);
                right.content = right_c.clone();
                right.alpha.snap(1.0);
            } else if let Some(content) = center.content.clone() {
                let mut rect = RectSpring::new(from);
                rect.set_target(right_target);
                let mut alpha = Spring::new(1.0);
                alpha.target = 0.0;
                self.ghost = Some(Ghost {
                    rect,
                    content,
                    alpha,
                });
            }
        }

        center.set(center_target, Some(center_c));
        right.set(right_target, right_c);
        self.center_id = center_id;
        if !self.motion.enabled {
            self.islands_mut().for_each(Island::finish);
            self.ghost = None;
        }
    }

    fn snap(
        &mut self,
        width: f32,
        center_target: Rect,
        center_c: Content,
        right_target: Rect,
        right_c: Option<Content>,
        center_id: Option<String>,
    ) {
        self.width = width;
        self.center = Some(Island::new(center_target, Some(center_c)));
        self.right = Some(Island::new(right_target, right_c));
        self.ghost = None;
        self.center_id = center_id;
    }

    pub fn step(&mut self, dt: f32) {
        let motion = self.motion;
        self.islands_mut().for_each(|i| i.step(dt, motion));
        if let Some(ghost) = &mut self.ghost {
            ghost.rect.step(dt, motion);
            ghost.alpha.step(dt, ghost_fade());
            if ghost.rect.settled() {
                self.ghost = None;
            }
        }
    }

    pub fn animating(&self) -> bool {
        self.ghost.is_some() || self.islands().any(|i| !i.settled())
    }

    fn islands(&self) -> impl Iterator<Item = &Island> {
        self.right.iter().chain(&self.center)
    }

    fn islands_mut(&mut self) -> impl Iterator<Item = &mut Island> {
        self.right.iter_mut().chain(&mut self.center)
    }

    /// Which island is under `(x, y)`, in surface coordinates.
    pub fn hit(&self, x: f32, y: f32) -> Option<IslandKind> {
        let inside =
            |i: &Option<Island>| i.as_ref().is_some_and(|i| i.rect.current().contains(x, y));
        if inside(&self.center) {
            Some(IslandKind::Center)
        } else if inside(&self.right) {
            Some(IslandKind::Right)
        } else {
            None
        }
    }

    /// Canvas height that fits both where islands are and where they're heading, so the
    /// canvas grows before an expansion starts and shrinks only after a collapse ends.
    pub fn canvas_height(&self, theme: &Theme) -> u32 {
        let rects = self
            .islands()
            .map(|i| &i.rect)
            .chain(self.ghost.as_ref().map(|g| &g.rect))
            .flat_map(|r| [r.current(), r.target()]);
        let bottom = rects.map(|r| r.bottom()).fold(0.0_f32, f32::max);
        ((bottom + theme.pill_top).ceil() as u32).max(theme.strip_height)
    }

    /// Updates the pointer position; returns whether the highlighted target changed.
    pub fn set_pointer(&mut self, pointer: Option<(f32, f32)>) -> bool {
        let before = self.hovered_hit().map(|(_, h)| h.rect);
        self.pointer = pointer;
        before != self.hovered_hit().map(|(_, h)| h.rect)
    }

    /// What clicking at `(x, y)` would do.
    pub fn target_at(&self, x: f32, y: f32) -> Option<Target> {
        self.hit_at(x, y).map(|(_, hit)| hit.target)
    }

    /// Starts (or stops) keyboard navigation of an island, selecting its first target.
    pub fn set_keyboard(&mut self, island: Option<IslandKind>) {
        self.keyboard = island;
        self.selection = 0;
    }

    pub fn keyboard(&self) -> Option<IslandKind> {
        self.keyboard
    }

    /// Moves the keyboard selection, wrapping around; returns whether it changed.
    pub fn move_selection(&mut self, delta: i32) -> bool {
        let n = self.keyboard_hits().len();
        if n == 0 {
            return false;
        }
        let current = self.selection.min(n - 1);
        self.selection = (current as i32 + delta).rem_euclid(n as i32) as usize;
        self.selection != current
    }

    /// What Enter would do.
    pub fn selected_target(&self) -> Option<Target> {
        self.selected_hit().map(|(_, hit)| hit.target)
    }

    fn selected_hit(&self) -> Option<(usize, Hit)> {
        let hits = self.keyboard_hits();
        let i = self.selection.min(hits.len().checked_sub(1)?);
        hits.into_iter().nth(i)
    }

    /// Keyboard-reachable hits of the navigated island, in surface coordinates.
    fn keyboard_hits(&self) -> Vec<(usize, Hit)> {
        let index = match self.keyboard {
            Some(IslandKind::Right) => 0,
            Some(IslandKind::Center) => 1,
            None => return Vec::new(),
        };
        let Some(island) = self.islands().nth(index) else {
            return Vec::new();
        };
        let Some(content) = &island.content else {
            return Vec::new();
        };
        let (dx, dy) = content_offset(island.rect.current(), content);
        content
            .hits
            .iter()
            .filter(|h| h.keyboard)
            .map(|h| {
                let rect = h.rect.translate(dx, dy);
                (index, Hit { rect, ..h.clone() })
            })
            .collect()
    }

    fn hovered_hit(&self) -> Option<(usize, Hit)> {
        if self.keyboard.is_some() {
            return self.selected_hit().filter(|(_, h)| h.highlight);
        }
        let (x, y) = self.pointer?;
        self.hit_at(x, y).filter(|(_, h)| h.highlight)
    }

    /// The hit under `(x, y)` in surface coordinates, with the index of its island
    /// (0 = right, 1 = center). Content that is still mostly faded out isn't clickable.
    fn hit_at(&self, x: f32, y: f32) -> Option<(usize, Hit)> {
        self.islands().enumerate().find_map(|(i, island)| {
            let rect = island.rect.current();
            let content = island
                .content
                .as_ref()
                .filter(|_| island.alpha.value > 0.5)?;
            if !rect.contains(x, y) {
                return None;
            }
            let (dx, dy) = content_offset(rect, content);
            content.hits.iter().find_map(|hit| {
                let r = hit.rect.translate(dx, dy);
                r.contains(x, y).then(|| {
                    (
                        i,
                        Hit {
                            rect: r,
                            ..hit.clone()
                        },
                    )
                })
            })
        })
    }

    pub fn scene(&self, theme: &Theme) -> Scene {
        let mut scene = Scene::default();
        let hovered = self.hovered_hit();
        for (i, island) in self.islands().enumerate() {
            let rect = island.rect.current();
            let highlight = hovered
                .as_ref()
                .filter(|(h, _)| *h == i)
                .map(|(_, hit)| hit.rect);
            push_island(
                &mut scene,
                theme,
                rect,
                island.content.as_ref(),
                island.alpha.value,
                highlight,
            );
            if rect.w >= 1.0 && rect.h >= 1.0 {
                scene.islands.push(rect);
            }
        }
        if let Some(g) = &self.ghost {
            push_island(
                &mut scene,
                theme,
                g.rect.current(),
                Some(&g.content),
                g.alpha.value,
                None,
            );
        }
        scene
    }
}

/// Where the right island shrinks to when it has nothing to show.
fn hidden_right(width: f32, theme: &Theme) -> Rect {
    let h = theme.pill_height;
    Rect::new(
        width - theme.right_margin - h / 2.0,
        theme.pill_top + h / 2.0,
        0.0,
        0.0,
    )
}

/// Where content's origin goes: centered in the pill, which may be mid-morph.
fn content_offset(rect: Rect, content: &Content) -> (f32, f32) {
    (
        rect.x + (rect.w - content.w) / 2.0,
        rect.y + (rect.h - content.h) / 2.0,
    )
}

fn push_island(
    scene: &mut Scene,
    theme: &Theme,
    rect: Rect,
    content: Option<&Content>,
    alpha: f32,
    highlight: Option<Rect>,
) {
    if rect.w < 0.5 || rect.h < 0.5 {
        return;
    }
    let pill = RoundRect {
        rect,
        radius: theme.radius(rect),
        color: theme.pill_color,
    };
    scene.groups.push(Group {
        clip: None,
        alpha: 1.0,
        items: vec![Draw::Rect(pill)],
    });
    let alpha = alpha.clamp(0.0, 1.0);
    let Some(content) = content.filter(|_| alpha > 0.003) else {
        return;
    };
    // Content stays centered in the pill while it morphs; the clip hides any overflow.
    let (dx, dy) = content_offset(rect, content);
    let mut items: Vec<Draw> = highlight
        .map(|r| {
            Draw::Rect(RoundRect {
                rect: r,
                radius: 10.0,
                color: theme.highlight,
            })
        })
        .into_iter()
        .collect();
    items.extend(content.items.iter().map(|d| {
        let mut d = d.clone();
        d.translate(dx, dy);
        d
    }));
    scene.groups.push(Group {
        clip: Some(rect),
        alpha,
        items,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tests::{FakeMeasure, activity, view};

    const W: f32 = 1645.0;
    const DT: f32 = 1.0 / 120.0;

    fn settle(stage: &mut Stage) -> u32 {
        let mut frames = 0;
        while stage.animating() {
            stage.step(DT);
            frames += 1;
            assert!(frames < 600, "did not settle within 5 s");
        }
        frames
    }

    fn update(stage: &mut Stage, v: &View, hover: Option<IslandKind>) {
        stage.update(W, v, hover, &Theme::default(), &mut FakeMeasure);
    }

    #[test]
    fn spring_settles_with_small_overshoot() {
        let mut s = Spring::new(0.0);
        s.target = 100.0;
        let mut peak = 0.0_f32;
        for _ in 0..240 {
            s.step(DT, morph(Motion::default()));
            peak = peak.max(s.value);
        }
        assert!(s.settled() && s.value == 100.0);
        assert!(peak > 100.0 && peak < 110.0, "peak {peak}");
    }

    #[test]
    fn spring_is_stable_with_long_frames() {
        let mut s = Spring::new(0.0);
        s.target = 1.0;
        s.step(0.5, morph(Motion::default()));
        assert!(s.value.is_finite() && (s.value - 1.0).abs() < 0.2);
    }

    #[test]
    fn first_update_snaps() {
        let mut stage = Stage::default();
        update(&mut stage, &view(None, &[]), None);
        assert!(!stage.animating());
        assert_eq!(stage.scene(&Theme::default()).islands.len(), 1);
    }

    #[test]
    fn new_activity_morphs_and_settles_at_content_size() {
        let theme = Theme::default();
        let mut stage = Stage::default();
        update(&mut stage, &view(None, &[]), None);
        let v = view(Some(activity("n", None)), &[]);
        update(&mut stage, &v, None);
        assert!(stage.animating());

        let frames = settle(&mut stage);
        assert!(frames > 20, "should visibly animate");
        let c = center_content(&v, false, &theme, &mut FakeMeasure);
        let rect = stage.center.as_ref().unwrap().rect.current();
        assert_eq!((rect.w, rect.x), (c.w, (W - c.w) / 2.0));
    }

    #[test]
    fn content_change_fades_in() {
        let mut stage = Stage::default();
        update(&mut stage, &view(None, &[]), None);
        update(&mut stage, &view(Some(activity("n", None)), &[]), None);
        assert_eq!(stage.center.as_ref().unwrap().alpha.value, 0.0);
        settle(&mut stage);
        assert_eq!(stage.center.as_ref().unwrap().alpha.value, 1.0);

        // A progress update to the same activity doesn't fade again.
        update(&mut stage, &view(Some(activity("n", Some(0.5))), &[]), None);
        assert_eq!(stage.center.as_ref().unwrap().alpha.value, 1.0);
    }

    #[test]
    fn move_to_hidden_right_flies_from_center() {
        let mut stage = Stage::default();
        let n = activity("n", None);
        update(&mut stage, &view(Some(n.clone()), &[]), None);
        let center_rect = stage.center.as_ref().unwrap().rect.current();

        update(&mut stage, &view(None, std::slice::from_ref(&n)), None);
        let right = stage.right.as_ref().unwrap();
        assert_eq!(right.rect.current(), center_rect);
        assert_eq!(right.alpha.value, 1.0);
        assert!(stage.ghost.is_none());
        settle(&mut stage);
        let right = stage.right.as_ref().unwrap().rect.current();
        assert!(right.x > W / 2.0, "ends up on the right");
    }

    #[test]
    fn move_to_visible_right_uses_a_ghost() {
        let mut stage = Stage::default();
        let (old, n) = (activity("old", None), activity("n", None));
        update(
            &mut stage,
            &view(Some(n.clone()), std::slice::from_ref(&old)),
            None,
        );
        update(&mut stage, &view(None, &[n, old]), None);
        assert!(stage.ghost.is_some());
        settle(&mut stage);
        assert!(stage.ghost.is_none());
    }

    #[test]
    fn right_hides_when_empty() {
        let mut stage = Stage::default();
        update(&mut stage, &view(None, &[activity("a", None)]), None);
        update(&mut stage, &view(None, &[]), None);
        settle(&mut stage);
        let scene = stage.scene(&Theme::default());
        assert_eq!(scene.islands.len(), 1, "only the center remains");
        assert!(stage.right.as_ref().unwrap().content.is_none());
    }

    #[test]
    fn clicks_and_highlights_follow_the_animated_content() {
        use crate::activity::{Action, ActivityView};
        let theme = Theme::default();
        let mut stage = Stage::default();
        let a = ActivityView {
            actions: vec![Action::new("allow", "Allow"), Action::new("deny", "Deny")],
            ..activity("perm", None)
        };
        let v = view(Some(a), &[]);
        update(&mut stage, &v, Some(IslandKind::Center));
        // Snapped (first update), so the expanded content is in place.
        let content = center_content(&v, true, &theme, &mut FakeMeasure);
        let rect = stage.center.as_ref().unwrap().rect.current();
        let (dx, dy) = content_offset(rect, &content);
        let allow = content.hits[0].rect.translate(dx, dy);
        let (x, y) = (allow.x + 2.0, allow.y + 2.0);

        let target = stage.target_at(x, y);
        let expected = Target::Invoke {
            id: "perm".into(),
            key: "allow".into(),
        };
        assert_eq!(target, Some(expected));
        assert!(stage.set_pointer(Some((x, y))), "highlight appears");
        assert!(!stage.set_pointer(Some((x + 1.0, y))), "same button");
        let highlights = stage
            .scene(&theme)
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .filter(|d| matches!(d, Draw::Rect(r) if r.color == theme.highlight))
            .count();
        assert_eq!(highlights, 1);
        assert_eq!(stage.target_at(5.0, 5.0), None);
    }

    #[test]
    fn keyboard_selection_wraps_and_skips_close_buttons() {
        let theme = Theme::default();
        let mut stage = Stage::default();
        let items = [activity("a", None), activity("b", None)];
        let v = view(None, &items);
        update(&mut stage, &v, Some(IslandKind::Right));
        stage.set_keyboard(Some(IslandKind::Right));
        // Rows a, b, then "Clear all"; close buttons are skipped.
        assert_eq!(
            stage.selected_target(),
            Some(Target::Dismiss { id: "a".into() })
        );
        assert!(stage.move_selection(1));
        assert_eq!(
            stage.selected_target(),
            Some(Target::Dismiss { id: "b".into() })
        );
        stage.move_selection(1);
        assert_eq!(stage.selected_target(), Some(Target::ClearRight));
        stage.move_selection(1);
        assert_eq!(
            stage.selected_target(),
            Some(Target::Dismiss { id: "a".into() }),
            "wraps"
        );
        stage.move_selection(-1);
        assert_eq!(
            stage.selected_target(),
            Some(Target::ClearRight),
            "wraps back"
        );
        let highlighted = stage
            .scene(&theme)
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .any(|d| matches!(d, Draw::Rect(r) if r.color == theme.highlight));
        assert!(highlighted, "selection is highlighted without a pointer");
        stage.set_keyboard(None);
        assert_eq!(stage.selected_target(), None);
    }

    #[test]
    fn motion_off_snaps() {
        let theme = Theme {
            motion: Motion {
                enabled: false,
                ..Motion::default()
            },
            ..Theme::default()
        };
        let mut stage = Stage::default();
        stage.update(W, &view(None, &[]), None, &theme, &mut FakeMeasure);
        let n = activity("n", None);
        stage.update(W, &view(Some(n), &[]), None, &theme, &mut FakeMeasure);
        assert!(!stage.animating());
        assert_eq!(stage.center.as_ref().unwrap().alpha.value, 1.0);
    }

    #[test]
    fn hover_expands_and_canvas_grows_before_animating() {
        let theme = Theme::default();
        let mut stage = Stage::default();
        let v = view(Some(activity("n", Some(0.4))), &[]);
        update(&mut stage, &v, None);
        assert_eq!(stage.canvas_height(&theme), theme.strip_height);

        let x = W / 2.0;
        assert_eq!(stage.hit(x, 10.0), Some(IslandKind::Center));
        update(&mut stage, &v, Some(IslandKind::Center));
        let expanded = stage.canvas_height(&theme);
        assert!(
            expanded > theme.strip_height,
            "sized for the target already"
        );

        settle(&mut stage);
        update(&mut stage, &v, None);
        assert_eq!(
            stage.canvas_height(&theme),
            expanded,
            "stays tall while collapsing"
        );
        settle(&mut stage);
        assert_eq!(stage.canvas_height(&theme), theme.strip_height);
    }
}
