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

/// Most activities kept at once; pushes beyond this are rejected.
pub const MAX_ACTIVITIES: usize = 256;
const MAX_TEXT_LEN: usize = 1024;

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
        if too_long(&self.id) || too_long(&self.title) || self.body.as_deref().is_some_and(too_long)
        {
            return Err(format!(
                "id, title and body are limited to {MAX_TEXT_LEN} bytes"
            ));
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
    pub priority: Priority,
}

/// The right island: the newest item plus how many more are waiting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RightView {
    pub newest: ActivityView,
    pub more: usize,
}

/// Everything both islands show at one moment. `None` means idle (center) or hidden (right).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct View {
    pub center: Option<ActivityView>,
    pub right: Option<RightView>,
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

    fn island(&self, now: Instant) -> Island {
        match self.spec.placement {
            Placement::Center => Island::Center,
            Placement::Right => Island::Right,
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
            priority: self.spec.priority,
        }
    }
}

/// The set of live activities. All methods take `now` explicitly so behavior is
/// deterministic and testable.
#[derive(Clone, Debug, Default)]
pub struct Activities {
    entries: Vec<Entry>,
}

impl Activities {
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

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Drops expired activities; returns whether anything was removed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|e| e.expires_at().is_none_or(|t| now < t));
        self.entries.len() != before
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
        let live = || {
            self.entries
                .iter()
                .filter(move |e| e.expires_at().is_none_or(|t| now < t))
        };

        let center = live()
            .filter(|e| e.island(now) == Island::Center)
            .max_by_key(|e| (e.spec.priority, e.is_transient(), e.updated))
            .map(Entry::view);

        let mut right: Vec<&Entry> = live().filter(|e| e.island(now) == Island::Right).collect();
        // Newest first; a flow activity "arrives" on the right when it leaves the center.
        right.sort_by_key(|e| {
            std::cmp::Reverse(e.moves_right_at().unwrap_or(e.created).max(e.updated))
        });
        let right = right.first().map(|newest| RightView {
            newest: newest.view(),
            more: right.len() - 1,
        });

        View { center, right }
    }

    pub fn states(&self, now: Instant) -> Vec<ActivityState> {
        self.entries
            .iter()
            .map(|e| ActivityState {
                spec: e.spec.clone(),
                island: e.island(now),
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
        assert_eq!(a.view(later).right.unwrap().newest.id, "n");
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

        assert!(!a.expire(t0 + ms(2000)));
        assert!(a.expire(t0 + ms(2500)));
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
    fn right_shows_newest_and_count() {
        let t0 = Instant::now();
        let mut a = Activities::default();
        a.push(spec("wifi", Placement::Right), t0).unwrap();
        a.push(spec("tg", Placement::Right), t0 + ms(10)).unwrap();
        let right = a.view(t0 + ms(10)).right.unwrap();
        assert_eq!((right.newest.id.as_str(), right.more), ("tg", 1));
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
    fn spec_json_uses_defaults() {
        let s: ActivitySpec = serde_json::from_str(r#"{"id":"a","title":"b"}"#).unwrap();
        assert_eq!(s, ActivitySpec::new("a", "b"));
    }
}
