//! Pure shell logic: activities, the arbiter, IPC types, geometry, theme and layout.
//! No I/O, no Wayland.

pub mod activity;
pub mod ipc;
mod layout;

pub use activity::{Activities, ActivitySpec, ActivityView, Placement, Priority, View};
pub use layout::{Color, Layout, Rect, RoundRect, Scene, Theme};
