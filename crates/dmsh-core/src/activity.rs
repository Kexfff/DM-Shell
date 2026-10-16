//! Activities and the arbiter that decides what each island shows.
//!
//! Lifecycle rules:
//! - `ttl_ms` counts from the last push of that id, so a repeatedly pushed volume OSD stays
//!   up while it keeps changing.
//! - The center phase of a `flow` activity counts from its first push, so updating a
//!   notification doesn't pop it back into the center.

use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The action invoked by clicking an activity itself rather than one of its buttons.
pub const DEFAULT_ACTION: &str = "default";
/// Most actions per activity.
pub const MAX_ACTIONS: usize = 8;

/// Something the user can do with an activity: a button, or [`DEFAULT_ACTION`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Action {
    pub key: String,
    /// Button text; may be empty for icon-only buttons.
    #[serde(default)]
    pub label: String,
    /// Built-in icon name for icon buttons (e.g. media controls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl Action {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            icon: None,
        }
    }
}

/// Most activities kept at once; pushes beyond this are rejected.
pub const MAX_ACTIVITIES: usize = 256;
/// Longest id, title or body accepted, in bytes.
pub const MAX_TEXT_LEN: usize = 1024;

/// Where an activity is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Center island only (OSDs, media, agent asking for permission).
    Center,
    /// Right island only (Wi-Fi changes, ambient info).
    Right,
    /// Center for `center_ms`, then the right island (notifications).
    #[default]
    Flow,
}

/// Higher priority wins the center island.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

/// What a client sends to create or update an activity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ActivitySpec {
    /// Stable identifier; pushing the same id again updates the activity.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Progress from 0.0 to 1.0, drawn as a bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    /// Built-in icon name, e.g. `bell`, `volume`, `terminal` (see `dmsh icons`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// A picture shown instead of the icon: an image file path, an icon-theme name
    /// (`firefox`), or an image registered by a source. Takes precedence over `icon`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Who it's from (e.g. the app that sent a notification); shown when expanded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Buttons, plus optionally a `default` action for clicking the activity itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// Stay after an action is invoked (media controls); otherwise invoking dismisses.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_on_action: bool,
    #[serde(default)]
    pub placement: Placement,
    #[serde(default)]
    pub priority: Priority,
    /// Lifetime after the last push; absent means until dismissed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    /// How long a `flow` activity stays in the center before moving right.
    #[serde(default = "default_center_ms")]
    pub center_ms: u64,
}

fn default_center_ms() -> u64 {
    2000
}

impl ActivitySpec {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            body: None,
            progress: None,
            icon: None,
            image: None,
            app: None,
            actions: Vec::new(),
            keep_on_action: false,
            placement: Placement::default(),
            priority: Priority::default(),
            ttl_ms: None,
            center_ms: default_center_ms(),
        }
    }

    fn validate(&mut self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("id must not be empty".into());
        }
        let too_long = |s: &str| s.len() > MAX_TEXT_LEN;
        let long_field = [&self.body, &self.image, &self.app]
            .into_iter()
            .any(|f| f.as_deref().is_some_and(too_long));
        if too_long(&self.id) || too_long(&self.title) || long_field {
            return Err(format!("text fields are limited to {MAX_TEXT_LEN} bytes"));
        }
        if self.actions.len() > MAX_ACTIONS {
            return Err(format!("at most {MAX_ACTIONS} actions"));
        }
        if self
            .actions
            .iter()
            .any(|a| a.key.is_empty() || too_long(&a.key) || too_long(&a.label))
        {
            return Err("action keys must be non-empty; keys and labels are length-limited".into());
        }
        if let Some(p) = self.progress {
            if !p.is_finite() {
                return Err("progress must be a finite number".into());
            }
            self.progress = Some(p.clamp(0.0, 1.0));
        }
        Ok(())
    }
}

/// Which island an activity currently belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Island {
    Center,
    Right,
}

/// What an island displays for one activity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ActivityView {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Renderer image key (resolved from the spec's `image`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    pub priority: Priority,
}

impl ActivityView {
    pub fn has_action(&self, key: &str) -> bool {
        self.actions.iter().any(|a| a.key == key)
    }
}

/// Most right-island items carried in a view (the expanded list scrolls through these).
pub const RIGHT_ITEMS: usize = 32;

/// The right island: the newest items, newest first, and how many there are in total.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RightView {
    /// Never empty; at most [`RIGHT_ITEMS`].
    pub items: Vec<ActivityView>,
    pub total: usize,
}

impl RightView {
    pub fn newest(&self) -> &ActivityView {
        &self.items[0]
    }
}

/// Everything both islands show at one moment. `None` means idle (center) or hidden (right).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct View {
    pub center: Option<ActivityView>,
    pub right: Option<RightView>,
    /// Idle center content, e.g. `13:08`. Filled in by the shell, not the arbiter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<String>,
    /// Shown when the idle center expands, e.g. `Thursday, 15 October`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// System tray icons, shown on the right island.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tray: Vec<TrayIconView>,
    /// An open tray menu; the right island shows it as a list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu: Option<MenuView>,
    /// Laptop battery, always shown on the right island; `None` without a battery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<BatteryView>,
    /// Quick controls in the expanded right island.
    #[serde(default)]
    pub controls: Controls,
    /// First list row shown in the expanded right island (UI state, per output).
    #[serde(skip)]
    pub right_scroll: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BatteryView {
    pub percent: u8,
    pub charging: bool,
    pub level: BatteryLevel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatteryLevel {
    #[default]
    Normal,
    Low,
    Critical,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Controls {
    /// Wi-Fi radio on/off; `None` when there is no Wi-Fi device (or NetworkManager).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi: Option<bool>,
    /// Idle (screen off, lock, suspend) is inhibited.
    pub keep_awake: bool,
    /// Do not disturb: notifications go straight to the right island.
    #[serde(default)]
    pub dnd: bool,
}

/// A quick control in the expanded right island.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Control {
    Wifi,
    KeepAwake,
    /// Do not disturb.
    Dnd,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrayIconView {
    /// The item's bus address.
    pub id: String,
    pub title: String,
    /// Image key registered with the renderer; `None` draws a placeholder dot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_attention: bool,
}

/// One level of a tray menu.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MenuView {
    pub title: String,
    pub entries: Vec<MenuEntryView>,
    /// A submenu: show a way back.
    pub nested: bool,
    /// First entry shown; long menus scroll.
    #[serde(default)]
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MenuEntryView {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    /// `Some(checked)` for check and radio items.
    pub toggle: Option<bool>,
    pub submenu: bool,
}

/// One live activity as reported by `state`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ActivityState {
    #[serde(flatten)]
    pub spec: ActivitySpec,
    pub island: Island,
    pub age_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_ms: Option<u64>,
}

#[derive(Clone, Debug)]
struct Entry {
    spec: ActivitySpec,
    created: Instant,
    updated: Instant,
}

impl Entry {
    fn expires_at(&self) -> Option<Instant> {
        self.spec
            .ttl_ms
            .map(|ms| self.updated + Duration::from_millis(ms))
    }

    fn moves_right_at(&self) -> Option<Instant> {
        (self.spec.placement == Placement::Flow)
            .then(|| self.created + Duration::from_millis(self.spec.center_ms))
    }

    /// Where the activity is now. With `quiet` (do not disturb), `flow` activities skip
    /// the center unless they're critical.
    fn island(&self, now: Instant, quiet: bool) -> Island {
        match self.spec.placement {
            Placement::Center => Island::Center,
            Placement::Right => Island::Right,
            Placement::Flow if quiet && self.spec.priority < Priority::Critical => Island::Right,
            Placement::Flow if self.moves_right_at().is_some_and(|t| now < t) => Island::Center,
            Placement::Flow => Island::Right,
        }
    }

    /// Transient activities (they will leave the center on their own) beat persistent ones
    /// of the same priority, so an OSD shows over media.
    fn is_transient(&self) -> bool {
        self.spec.ttl_ms.is_some() || self.spec.placement == Placement::Flow
    }

    fn view(&self) -> ActivityView {
        ActivityView {
            id: self.spec.id.clone(),
            title: self.spec.title.clone(),
            body: self.spec.body.clone(),
            progress: self.spec.progress,
            icon: self.spec.icon.clone(),
            image: self.spec.image.clone(),
            app: self.spec.app.clone(),
            actions: self.spec.actions.clone(),
            priority: self.spec.priority,
        }
    }
}

/// The set of live activities. All methods take `now` explicitly so behavior is
/// deterministic and testable.
#[derive(Clone, Debug, Default)]
pub struct Activities {
    entries: Vec<Entry>,
    /// Do not disturb.
    quiet: bool,
}

impl Activities {
    /// Turns do-not-disturb on or off.
    pub fn set_quiet(&mut self, quiet: bool) {
        self.quiet = quiet;
    }

    /// Creates or updates an activity.
    pub fn push(&mut self, mut spec: ActivitySpec, now: Instant) -> Result<(), String> {
        spec.validate()?;
        if let Some(entry) = self.entries.iter_mut().find(|e| e.spec.id == spec.id) {
            entry.spec = spec;
            entry.updated = now;
            return Ok(());
        }
        if self.entries.len() >= MAX_ACTIVITIES {
            return Err(format!("too many activities (max {MAX_ACTIVITIES})"));
        }
        self.entries.push(Entry {
            spec,
            created: now,
            updated: now,
        });
        Ok(())
    }

    /// Removes an activity; returns whether it existed.
    pub fn dismiss(&mut self, id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.spec.id != id);
        self.entries.len() != before
    }

    /// Whether `id` has an action `key`; `Some(keep_on_action)` if it does.
    pub fn action(&self, id: &str, key: &str) -> Option<bool> {
        let entry = self.entries.iter().find(|e| e.spec.id == id)?;
        let has = entry.spec.actions.iter().any(|a| a.key == key);
        has.then_some(entry.spec.keep_on_action)
    }

    /// Image keys referenced by live activities (so unused images can be dropped).
    pub fn image_keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().filter_map(|e| e.spec.image.as_deref())
    }

    /// Removes everything currently on one island; returns the removed ids.
    pub fn clear_island(&mut self, island: Island, now: Instant) -> Vec<String> {
        let (removed, rest) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|e| e.island(now, self.quiet) == island);
        self.entries = rest;
        removed.into_iter().map(|e: Entry| e.spec.id).collect()
    }

    /// Removes everything; returns the removed ids.
    pub fn clear(&mut self) -> Vec<String> {
        self.entries.drain(..).map(|e| e.spec.id).collect()
    }

    /// Drops expired activities; returns whether anything was removed.
    /// Drops expired activities; returns their ids.
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let (live, expired) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|e| e.expires_at().is_none_or(|t| now < t));
        self.entries = live;
        expired.into_iter().map(|e: Entry| e.spec.id).collect()
    }

    /// The next moment the view may change without any new input.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.entries
            .iter()
            .flat_map(|e| [e.expires_at(), e.moves_right_at()])
            .flatten()
            .filter(|&t| t > now)
            .min()
    }

    pub fn view(&self, now: Instant) -> View {
        self.view_for(now, true)
    }

    /// The view for one output. Transient center activities (notifications, OSDs) appear
    /// only on the focused output; other outputs fall back to persistent ones or idle.
    pub fn view_for(&self, now: Instant, focused: bool) -> View {
        let live = || {
            self.entries
                .iter()
                .filter(move |e| e.expires_at().is_none_or(|t| now < t))
        };

        let center = live()
            .filter(|e| e.island(now, self.quiet) == Island::Center)
            .filter(|e| focused || !e.is_transient())
            .max_by_key(|e| (e.spec.priority, e.is_transient(), e.updated))
            .map(Entry::view);

        let mut right: Vec<&Entry> = live()
            .filter(|e| e.island(now, self.quiet) == Island::Right)
            .collect();
        // Newest first; a flow activity "arrives" on the right when it leaves the center.
        right.sort_by_key(|e| {
            std::cmp::Reverse(e.moves_right_at().unwrap_or(e.created).max(e.updated))
        });
        let right = (!right.is_empty()).then(|| RightView {
            items: right.iter().take(RIGHT_ITEMS).map(|e| e.view()).collect(),
            total: right.len(),
        });

        View {
            center,
            right,
            clock: None,
            date: None,
            tray: Vec::new(),
            menu: None,
            battery: None,
            controls: Controls::default(),
            right_scroll: 0,
        }
    }

    pub fn states(&self, now: Instant) -> Vec<ActivityState> {
        self.entries
            .iter()
            .map(|e| ActivityState {
                spec: e.spec.clone(),
                island: e.island(now, self.quiet),
                age_ms: now.saturating_duration_since(e.created).as_millis() as u64,
                expires_in_ms: e
                    .expires_at()
                    .map(|t| t.saturating_duration_since(now).as_millis() as u64),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn spec(id: &str, placement: Placement) -> ActivitySpec {
        ActivitySpec {
            placement,
            ..ActivitySpec::new(id, id)
        }
    }

    fn center_id(a: &Activities, now: Instant) -> Option<String> {
        a.view(now).center.map(|v| v.id)
    }

    #[test]
    fn flow_moves_from_center_to_right() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.push(spec("n", Placement::Flow), t0).unwrap();

        assert_eq!(center_id(&a, t0), Some("n".into()));
        assert!(a.view(t0).right.is_none());

        let later = t0 + ms(2000);
        assert_eq!(center_id(&a, later), None);
        assert_eq!(a.view(later).right.unwrap().newest().id, "n");
    }

    #[test]
    fn updating_flow_does_not_return_it_to_center() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.push(spec("n", Placement::Flow), t0).unwrap();
        a.push(spec("n", Placement::Flow), t0 + ms(3000)).unwrap();
        assert_eq!(center_id(&a, t0 + ms(3000)), None);
    }

    #[test]
    fn ttl_counts_from_last_push() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        let osd = ActivitySpec {
            ttl_ms: Some(1500),
            ..spec("vol", Placement::Center)
        };
        a.push(osd.clone(), t0).unwrap();
        a.push(osd, t0 + ms(1000)).unwrap();

        assert!(a.expire(t0 + ms(2000)).is_empty());
        assert_eq!(a.expire(t0 + ms(2500)), ["vol"]);
        assert_eq!(center_id(&a, t0 + ms(2500)), None);
    }

    #[test]
    fn expired_activities_are_hidden_before_expire_runs() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.push(
            ActivitySpec {
                ttl_ms: Some(100),
                ..spec("x", Placement::Center)
            },
            t0,
        )
        .unwrap();
        assert_eq!(center_id(&a, t0 + ms(100)), None);
    }

    #[test]
    fn priority_then_transience_wins_center() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.push(spec("media", Placement::Center), t0).unwrap();
        let osd = ActivitySpec {
            ttl_ms: Some(1500),
            ..spec("vol", Placement::Center)
        };
        a.push(osd, t0 - ms(1)).unwrap();
        // Same priority: the transient OSD beats persistent media even though it's older.
        assert_eq!(center_id(&a, t0), Some("vol".into()));

        let urgent = ActivitySpec {
            priority: Priority::Critical,
            ..spec("perm", Placement::Center)
        };
        a.push(urgent, t0).unwrap();
        assert_eq!(center_id(&a, t0), Some("perm".into()));
    }

    #[test]
    fn unfocused_outputs_skip_transient_center_activities() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.push(spec("media", Placement::Center), t0).unwrap();
        a.push(spec("notif", Placement::Flow), t0).unwrap();
        assert_eq!(a.view_for(t0, true).center.unwrap().id, "notif");
        assert_eq!(a.view_for(t0, false).center.unwrap().id, "media");
        // The right island is the same everywhere.
        let later = t0 + ms(2000);
        assert_eq!(
            a.view_for(later, false).right,
            a.view_for(later, true).right
        );
    }

    #[test]
    fn clear_returns_removed_ids() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.push(spec("a", Placement::Right), t0).unwrap();
        assert_eq!(a.clear(), ["a"]);
        assert!(a.view(t0).right.is_none());
    }

    #[test]
    fn quiet_sends_notifications_straight_right_except_critical() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.set_quiet(true);
        a.push(spec("n", Placement::Flow), t0).unwrap();
        assert_eq!(center_id(&a, t0), None);
        assert_eq!(a.view(t0).right.unwrap().newest().id, "n");

        let urgent = ActivitySpec {
            priority: Priority::Critical,
            ..spec("u", Placement::Flow)
        };
        a.push(urgent, t0).unwrap();
        assert_eq!(center_id(&a, t0), Some("u".into()));

        // Center-placed activities (OSDs, questions) are unaffected.
        a.push(spec("osd", Placement::Center), t0).unwrap();
        assert!(a.view(t0).center.is_some());
    }

    #[test]
    fn right_shows_newest_and_count() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.push(spec("wifi", Placement::Right), t0).unwrap();
        a.push(spec("tg", Placement::Right), t0 + ms(10)).unwrap();
        let right = a.view(t0 + ms(10)).right.unwrap();
        assert_eq!((right.newest().id.as_str(), right.total), ("tg", 2));
        assert_eq!(right.items[1].id, "wifi");
    }

    #[test]
    fn next_deadline_is_earliest_transition() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.push(spec("n", Placement::Flow), t0).unwrap();
        a.push(
            ActivitySpec {
                ttl_ms: Some(500),
                ..spec("v", Placement::Center)
            },
            t0,
        )
        .unwrap();
        assert_eq!(a.next_deadline(t0), Some(t0 + ms(500)));
        assert_eq!(a.next_deadline(t0 + ms(500)), Some(t0 + ms(2000)));
        assert_eq!(a.next_deadline(t0 + ms(2000)), None);
    }

    #[test]
    fn dismiss_and_validation() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        assert!(a.push(ActivitySpec::new("", "x"), t0).is_err());
        let nan = ActivitySpec {
            progress: Some(f32::NAN),
            ..ActivitySpec::new("p", "x")
        };
        assert!(a.push(nan, t0).is_err());
        let over = ActivitySpec {
            progress: Some(1.7),
            ..ActivitySpec::new("p", "x")
        };
        a.push(over, t0).unwrap();
        assert_eq!(a.states(t0)[0].spec.progress, Some(1.0));
        assert!(a.dismiss("p"));
        assert!(!a.dismiss("p"));
    }

    #[test]
    fn actions_lookup_and_clear_right() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        let with_action = ActivitySpec {
            actions: vec![Action::new("allow", "Allow")],
            ..spec("perm", Placement::Center)
        };
        a.push(with_action, t0).unwrap();
        a.push(spec("tg", Placement::Right), t0).unwrap();
        assert_eq!(a.action("perm", "allow"), Some(false));
        assert_eq!(a.action("perm", "deny"), None);
        assert_eq!(a.clear_island(Island::Right, t0), ["tg"]);
        assert_eq!(a.states(t0).len(), 1);

        let empty_key = ActivitySpec {
            actions: vec![Action::new("", "x")],
            ..spec("bad", Placement::Center)
        };
        assert!(a.push(empty_key, t0).is_err());
    }

    #[test]
    fn spec_json_uses_defaults() {
        let s: ActivitySpec = serde_json::from_str(r#"{"id":"a","title":"b"}"#).unwrap();
        assert_eq!(s, ActivitySpec::new("a", "b"));
    }
}
