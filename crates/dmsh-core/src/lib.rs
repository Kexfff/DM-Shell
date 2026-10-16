//! Pure shell logic: activities, the arbiter, IPC types, layout and animation.
//! No I/O, no Wayland, no fonts.

pub mod activity;
pub mod anim;
pub mod config;
pub mod ipc;
mod layout;

pub use activity::{
    Action, Activities, ActivitySpec, ActivityView, BatteryLevel, BatteryView, Control, Controls,
    DEFAULT_ACTION, MenuEntryView, MenuView, Placement, Priority, TrayIconView, View,
};
pub use anim::{IslandKind, Stage};
pub use layout::{
    Align, Color, Content, Draw, Group, Hit, IconDraw, ImageDraw, LIST_ROWS, MENU_ROWS, Measure,
    Motion, Rect, RoundRect, Scene, Target, TextDraw, TextStyle, Theme, Weight, center_content,
    right_content,
};
