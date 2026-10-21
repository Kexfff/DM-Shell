//! Built-in stroke icons on a 24×24 grid (2-unit round strokes), drawn with tiny-skia.
//! Path data is parsed once per icon and cached.

use std::collections::HashMap;

use dmsh_core::IconDraw;
use svgtypes::{SimplePathSegment, SimplifyingPathParser};
use tiny_skia::{LineCap, LineJoin, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};

const SPEAKER: &str = "M11 5 6 9H3a1 1 0 0 0-1 1v4a1 1 0 0 0 1 1h3l5 4z";
const BATTERY: &str = "M5 7h11a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2z";
const BATTERY_TIP: &str = "M22 11v2";

/// `(name, path data)`; several strings are drawn as separate subpaths.
const ICONS: &[(&str, &[&str])] = &[
    (
        "bell",
        &[
            "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9",
            "M10.3 21a1.94 1.94 0 0 0 3.4 0",
        ],
    ),
    (
        "volume",
        &[SPEAKER, "M15.5 8.5a5 5 0 0 1 0 7", "M19 5a10 10 0 0 1 0 14"],
    ),
    ("volume-mute", &[SPEAKER, "M22 9l-6 6", "M16 9l6 6"]),
    (
        "brightness",
        &[
            "M16 12a4 4 0 1 1-8 0a4 4 0 1 1 8 0",
            "M12 2v2",
            "M12 20v2",
            "M4.9 4.9l1.4 1.4",
            "M17.7 17.7l1.4 1.4",
            "M2 12h2",
            "M20 12h2",
            "M6.3 17.7l-1.4 1.4",
            "M19.1 4.9l-1.4 1.4",
        ],
    ),
    (
        "keyboard",
        &[
            "M4 5h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2z",
            "M6 9h.01",
            "M10 9h.01",
            "M14 9h.01",
            "M18 9h.01",
            "M6 13h.01",
            "M18 13h.01",
            "M10 13h4",
            "M7 16h10",
        ],
    ),
    (
        "music",
        &[
            "M9 18V5l12-2v13",
            "M9 18a3 3 0 1 1-6 0a3 3 0 1 1 6 0",
            "M21 16a3 3 0 1 1-6 0a3 3 0 1 1 6 0",
        ],
    ),
    ("battery", &[BATTERY, BATTERY_TIP]),
    ("battery-low", &[BATTERY, BATTERY_TIP, "M7 11v2"]),
    (
        "battery-charging",
        &[BATTERY, BATTERY_TIP, "M11 9l-2.5 3h3L9 15"],
    ),
    (
        "wifi",
        &[
            "M5 12.5a10 10 0 0 1 14 0",
            "M1.5 9a15 15 0 0 1 21 0",
            "M8.5 16a5 5 0 0 1 7 0",
            "M12 20h.01",
        ],
    ),
    (
        "wifi-off",
        &[
            "M2 2l20 20",
            "M8.5 16a5 5 0 0 1 7 0",
            "M5 12.5a10 10 0 0 1 5-2.7",
            "M12 20h.01",
        ],
    ),
    (
        "globe",
        &[
            "M22 12a10 10 0 1 1-20 0a10 10 0 1 1 20 0",
            "M2 12h20",
            "M12 2a15 15 0 0 1 0 20a15 15 0 0 1 0-20",
        ],
    ),
    (
        "screen-share",
        &[
            "M4 4h16a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z",
            "M8 21h8",
            "M12 17v4",
            "M9 10.5l3-3 3 3",
            "M12 7.5v6",
        ],
    ),
    (
        "alert",
        &[
            "M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z",
            "M12 9v4",
            "M12 17h.01",
        ],
    ),
    ("check", &["M20 6 9 17l-5-5"]),
    ("x", &["M18 6 6 18", "M6 6l12 12"]),
    ("chevron-left", &["M15 18l-6-6 6-6"]),
    ("chevron-up", &["M18 15l-6-6-6 6"]),
    ("chevron-down", &["M6 9l6 6 6-6"]),
    ("moon", &["M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9z"]),
    ("power", &["M12 2v10", "M18.4 6.6a9 9 0 1 1-12.77.04"]),
    (
        "mic",
        &[
            "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z",
            "M19 10v2a7 7 0 0 1-14 0v-2",
            "M12 19v3",
        ],
    ),
    (
        "mic-off",
        &[
            "M2 2l20 20",
            "M18.89 13.23A7 7 0 0 0 19 12v-2",
            "M5 10v2a7 7 0 0 0 12 5",
            "M15 9.34V5a3 3 0 0 0-5.68-1.33",
            "M9 9v3a3 3 0 0 0 5.12 2.12",
            "M12 19v3",
        ],
    ),
    (
        "camera",
        &[
            "M16 13l5.22 3.48a.5.5 0 0 0 .78-.42V7.87a.5.5 0 0 0-.75-.43L16 10.5",
            "M4 6h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2z",
        ],
    ),
    (
        "coffee",
        &[
            "M17 8h1a4 4 0 1 1 0 8h-1",
            "M3 8h14v9a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4z",
            "M6 2v2",
            "M10 2v2",
            "M14 2v2",
        ],
    ),
    ("chevron-right", &["M9 18l6-6-6-6"]),
    (
        "play",
        &["M7 4.5v15a1 1 0 0 0 1.5.9l12-7.5a1 1 0 0 0 0-1.8l-12-7.5A1 1 0 0 0 7 4.5z"],
    ),
    ("pause", &["M7 4v16", "M17 4v16"]),
    ("skip-back", &["M19 20 9 12l10-8z", "M5 19V5"]),
    ("skip-forward", &["M5 4l10 8-10 8z", "M19 5v14"]),
    ("terminal", &["M4 17l6-6-6-6", "M12 19h8"]),
    (
        "message",
        &["M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"],
    ),
    (
        "download",
        &[
            "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4",
            "M7 10l5 5 5-5",
            "M12 15V3",
        ],
    ),
];

/// Names of every built-in icon.
pub fn names() -> impl Iterator<Item = &'static str> {
    ICONS.iter().map(|(name, _)| *name)
}

#[derive(Default)]
pub struct Icons {
    paths: HashMap<String, Option<Path>>,
}

impl Icons {
    /// Draws an icon; unknown names draw nothing (logged once).
    pub fn draw(&mut self, icon: &IconDraw, scale: f32, alpha: f32, pixmap: &mut PixmapMut) {
        let path = self.paths.entry(icon.name.clone()).or_insert_with(|| {
            let path = ICONS
                .iter()
                .find(|(name, _)| *name == icon.name)
                .and_then(|(_, data)| parse(data));
            if path.is_none() {
                log::warn!("unknown icon `{}`", icon.name);
            }
            path
        });
        let Some(path) = path else { return };

        let s = icon.rect.w * scale / 24.0;
        let transform =
            Transform::from_row(s, 0.0, 0.0, s, icon.rect.x * scale, icon.rect.y * scale);
        let mut paint = Paint::default();
        let c = icon.color;
        let a = (c.a as f32 * alpha).round().clamp(0.0, 255.0) as u8;
        paint.set_color_rgba8(c.r, c.g, c.b, a);
        paint.anti_alias = true;
        let stroke = Stroke {
            width: 2.0,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(path, &paint, &stroke, transform, None);
    }
}

fn parse(data: &[&str]) -> Option<Path> {
    let mut pb = PathBuilder::new();
    for d in data {
        for segment in SimplifyingPathParser::from(*d) {
            match segment.ok()? {
                SimplePathSegment::MoveTo { x, y } => pb.move_to(x as f32, y as f32),
                SimplePathSegment::LineTo { x, y } => pb.line_to(x as f32, y as f32),
                SimplePathSegment::Quadratic { x1, y1, x, y } => {
                    pb.quad_to(x1 as f32, y1 as f32, x as f32, y as f32)
                }
                SimplePathSegment::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x,
                    y,
                } => pb.cubic_to(
                    x1 as f32, y1 as f32, x2 as f32, y2 as f32, x as f32, y as f32,
                ),
                SimplePathSegment::ClosePath => pb.close(),
            }
        }
    }
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses_inside_its_grid() {
        for (name, data) in ICONS {
            let path = parse(data).unwrap_or_else(|| panic!("{name} failed to parse"));
            let b = path.bounds();
            assert!(
                b.left() >= 0.0 && b.top() >= 0.0 && b.right() <= 24.0 && b.bottom() <= 24.0,
                "{name} outside 24x24: {b:?}"
            );
        }
    }

    #[test]
    fn names_are_unique() {
        let mut all: Vec<_> = names().collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), ICONS.len());
    }
}
