//! Pure shell logic: activities, the arbiter, IPC types, layout and animation.
//! No I/O, no Wayland, no fonts.

pub mod activity;
pub mod anim;
pub mod ipc;
mod layout;

pub use activity::{Activities, ActivitySpec, ActivityView, Placement, Priority, View};
pub use anim::{IslandKind, Stage};
pub use layout::{
    Align, Color, Content, Draw, Group, IconDraw, Measure, Rect, RoundRect, Scene, TextDraw,
    TextStyle, Theme, Weight, center_content, right_content,
};
