//! `~/.config/dmsh/config.kdl`: parsing and defaults.
//!
//! Parsing is strict (unknown sections and options are errors with line numbers) so typos
//! don't silently do nothing. Both KDL v2 and v1 (niri's syntax) are accepted. The
//! documented default config lives in `default-config.kdl`; a test keeps it in sync with
//! [`Config::default`].

use std::fmt;
use std::time::Duration;

use kdl::{KdlDocument, KdlNode, KdlValue};

use crate::activity::SessionAction;
use crate::layout::{Color, Theme};

/// The documented default config, printed by `dmsh config default`.
pub const DEFAULT_CONFIG: &str = include_str!("default-config.kdl");

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub theme: Theme,
    /// UI font family; `None` picks Inter if installed, else the system sans-serif.
    pub font: Option<String>,
    pub clock: Clock,
    pub outputs: Outputs,
    pub sources: Sources,
    pub timing: Timing,
    pub session: Session,
    pub weather: Weather,
}

/// The temperature on the right island, from wttr.in.
#[derive(Clone, Debug, PartialEq)]
pub struct Weather {
    /// A place wttr.in understands ("Berlin", "~Eiffel Tower", an airport code);
    /// `None` lets it guess from your IP address.
    pub location: Option<String>,
    /// `None` uses the place's usual units.
    pub units: Option<Units>,
    /// How often to check.
    pub interval: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    Celsius,
    Fahrenheit,
}

/// The session menu: how long to hold an option, and the commands it runs (via `sh -c`).
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub hold: Duration,
    pub poweroff: String,
    pub reboot: String,
    pub logout: String,
}

impl Session {
    pub fn command(&self, action: SessionAction) -> &str {
        match action {
            SessionAction::Poweroff => &self.poweroff,
            SessionAction::Reboot => &self.reboot,
            SessionAction::Logout => &self.logout,
        }
    }
}

/// strftime formats for the idle clock and the expanded date.
#[derive(Clone, Debug, PartialEq)]
pub struct Clock {
    pub time: String,
    pub date: String,
}

/// Which outputs get islands; empty `only` means all.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outputs {
    pub only: Vec<String>,
    pub except: Vec<String>,
}

impl Outputs {
    pub fn wants(&self, name: Option<&str>) -> bool {
        let Some(name) = name else {
            return self.only.is_empty();
        };
        (self.only.is_empty() || self.only.iter().any(|o| o == name))
            && !self.except.iter().any(|o| o == name)
    }
}

/// Which sources run (read at startup; changing them needs a restart).
#[derive(Clone, Debug, PartialEq)]
pub struct Sources {
    pub notifications: bool,
    pub tray: bool,
    pub media: bool,
    pub volume: bool,
    pub brightness: bool,
    pub power: bool,
    pub network: bool,
    pub keyboard_layout: bool,
    pub screencast: bool,
    pub microphone: bool,
    pub camera: bool,
    pub weather: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timing {
    /// Volume, brightness and keyboard-layout overlays.
    pub osd: Duration,
    /// Right-island status messages (Wi-Fi, charger).
    pub status: Duration,
    /// How long a notification stays in the center before moving right.
    pub notification_center: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            font: None,
            clock: Clock {
                time: "%H:%M".into(),
                date: "%A, %-d %B".into(),
            },
            outputs: Outputs::default(),
            sources: Sources {
                notifications: true,
                tray: true,
                media: true,
                volume: true,
                brightness: true,
                power: true,
                network: true,
                keyboard_layout: true,
                screencast: true,
                microphone: true,
                camera: true,
                weather: true,
            },
            timing: Timing {
                osd: Duration::from_millis(1500),
                status: Duration::from_secs(4),
                notification_center: Duration::from_secs(2),
            },
            session: Session {
                hold: Duration::from_millis(800),
                poweroff: "systemctl poweroff".into(),
                reboot: "systemctl reboot".into(),
                logout: "niri msg action quit --skip-confirmation".into(),
            },
            weather: Weather {
                location: None,
                units: None,
                interval: Duration::from_secs(20 * 60),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    /// 1-based; 0 when the position is unknown.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            0 => write!(f, "{}", self.message),
            line => write!(f, "line {line}: {}", self.message),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let doc = KdlDocument::parse(text).map_err(|e| {
            let first = e.diagnostics.first();
            ConfigError {
                line: first.map_or(0, |d| line_of(text, d.span.offset())),
                message: first
                    .and_then(|d| d.message.clone().or_else(|| d.label.clone()))
                    .unwrap_or_else(|| "invalid KDL".into()),
            }
        })?;
        let mut config = Config::default();
        let p = Parser { text };
        for node in doc.nodes() {
            match node.name().value() {
                "theme" => p.theme(node, &mut config)?,
                "clock" => p.clock(node, &mut config.clock)?,
                "animation" => p.animation(node, &mut config.theme)?,
                "outputs" => p.outputs(node, &mut config.outputs)?,
                "sources" => p.sources(node, &mut config.sources)?,
                "timing" => p.timing(node, &mut config.timing)?,
                "session" => p.session(node, &mut config.session)?,
                "weather" => p.weather(node, &mut config.weather)?,
                other => return Err(p.error(node, format!("unknown section `{other}`"))),
            }
        }
        let t = &mut config.theme;
        t.pill_top = ((t.strip_height as f32 - t.pill_height) / 2.0).max(0.0);
        Ok(config)
    }
}

/// Parses `1500ms`, `2s` or `5m`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let split = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let n: f64 = number
        .parse()
        .map_err(|_| format!("invalid duration `{s}`"))?;
    let secs = match unit {
        "ms" => n / 1000.0,
        "s" => n,
        "m" => n * 60.0,
        "" => return Err(format!("`{s}` needs a unit: ms, s or m")),
        _ => return Err(format!("unknown unit `{unit}` (use ms, s or m)")),
    };
    Duration::try_from_secs_f64(secs).map_err(|e| e.to_string())
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

struct Parser<'a> {
    text: &'a str,
}

impl Parser<'_> {
    fn error(&self, node: &KdlNode, message: impl Into<String>) -> ConfigError {
        ConfigError {
            line: line_of(self.text, node.span().offset()),
            message: message.into(),
        }
    }

    fn children<'n>(&self, node: &'n KdlNode) -> &'n [KdlNode] {
        node.children().map_or(&[], |c| c.nodes())
    }

    /// The node's single positional argument.
    fn arg<'n>(&self, node: &'n KdlNode) -> Result<&'n KdlValue, ConfigError> {
        let mut args = node.entries().iter().filter(|e| e.name().is_none());
        match (args.next(), args.next()) {
            (Some(a), None) => Ok(a.value()),
            _ => Err(self.error(node, format!("`{}` takes one value", node.name().value()))),
        }
    }

    fn string(&self, node: &KdlNode) -> Result<String, ConfigError> {
        match self.arg(node)? {
            KdlValue::String(s) => Ok(s.clone()),
            _ => Err(self.error(node, format!("`{}` expects a string", node.name().value()))),
        }
    }

    fn strings(&self, node: &KdlNode) -> Result<Vec<String>, ConfigError> {
        node.entries()
            .iter()
            .map(|e| match e.value() {
                KdlValue::String(s) if e.name().is_none() => Ok(s.clone()),
                _ => Err(self.error(node, "expects one or more output names")),
            })
            .collect()
    }

    fn number(&self, node: &KdlNode, min: f64, max: f64) -> Result<f32, ConfigError> {
        let v = match self.arg(node)? {
            KdlValue::Integer(i) => *i as f64,
            KdlValue::Float(f) => *f,
            _ => {
                return Err(self.error(node, format!("`{}` expects a number", node.name().value())));
            }
        };
        if !(min..=max).contains(&v) {
            return Err(self.error(
                node,
                format!("`{}` must be between {min} and {max}", node.name().value()),
            ));
        }
        Ok(v as f32)
    }

    /// `name true`, `name false`, or a bare `name` meaning true.
    fn flag(&self, node: &KdlNode) -> Result<bool, ConfigError> {
        if node.entries().is_empty() {
            return Ok(true);
        }
        match self.arg(node)? {
            KdlValue::Bool(b) => Ok(*b),
            _ => Err(self.error(
                node,
                format!("`{}` expects true or false", node.name().value()),
            )),
        }
    }

    fn color(&self, node: &KdlNode, value: &KdlValue) -> Result<Color, ConfigError> {
        let KdlValue::String(s) = value else {
            return Err(self.error(node, "colors are strings like \"#rrggbb\""));
        };
        parse_color(s).ok_or_else(|| {
            self.error(
                node,
                format!("invalid color `{s}` (use #rrggbb or #rrggbbaa)"),
            )
        })
    }

    fn duration(&self, node: &KdlNode) -> Result<Duration, ConfigError> {
        let s = self.string(node)?;
        parse_duration(&s).map_err(|e| self.error(node, e))
    }

    fn theme(&self, section: &KdlNode, config: &mut Config) -> Result<(), ConfigError> {
        let t = &mut config.theme;
        for node in self.children(section) {
            match node.name().value() {
                "font" => config.font = Some(self.string(node)?),
                "pill-color" => t.pill_color = self.color(node, self.arg(node)?)?,
                "text-color" => t.text = self.color(node, self.arg(node)?)?,
                "secondary-text-color" => t.text_secondary = self.color(node, self.arg(node)?)?,
                "accent-colors" => {
                    for entry in node.entries() {
                        let index = match entry.name().map(|n| n.value()) {
                            Some("low") => 0,
                            Some("normal") => 1,
                            Some("high") => 2,
                            Some("critical") => 3,
                            _ => {
                                return Err(self
                                    .error(node, "use low=, normal=, high= and critical= colors"));
                            }
                        };
                        t.priority_colors[index] = self.color(node, entry.value())?;
                    }
                }
                "pill-height" => t.pill_height = self.number(node, 16.0, 80.0)?,
                "reserved-height" => t.strip_height = self.number(node, 0.0, 120.0)? as u32,
                "corner-radius" => t.max_radius = self.number(node, 0.0, 100.0)?,
                "center-idle-width" => t.center_idle_width = self.number(node, 40.0, 1000.0)?,
                "center-min-width" => t.center_min_width = self.number(node, 40.0, 1000.0)?,
                "center-expanded-width" => {
                    t.center_expanded_width = self.number(node, 200.0, 1200.0)?
                }
                "right-expanded-width" => {
                    t.right_expanded_width = self.number(node, 200.0, 1200.0)?
                }
                "right-margin" => t.right_margin = self.number(node, 0.0, 400.0)?,
                other => return Err(self.error(node, format!("unknown theme option `{other}`"))),
            }
        }
        if t.pill_height > t.strip_height as f32 && t.strip_height > 0 {
            return Err(self.error(section, "pill-height can't exceed reserved-height"));
        }
        Ok(())
    }

    fn clock(&self, section: &KdlNode, clock: &mut Clock) -> Result<(), ConfigError> {
        for node in self.children(section) {
            let format = match node.name().value() {
                "format" => &mut clock.time,
                "date-format" => &mut clock.date,
                other => return Err(self.error(node, format!("unknown clock option `{other}`"))),
            };
            let value = self.string(node)?;
            let invalid = chrono::format::StrftimeItems::new(&value)
                .any(|item| matches!(item, chrono::format::Item::Error));
            if invalid {
                return Err(self.error(node, format!("invalid strftime format `{value}`")));
            }
            *format = value;
        }
        Ok(())
    }

    fn animation(&self, section: &KdlNode, theme: &mut Theme) -> Result<(), ConfigError> {
        for node in self.children(section) {
            match node.name().value() {
                "off" => theme.motion.enabled = !self.flag(node)?,
                "response" => theme.motion.response = self.number(node, 0.05, 3.0)?,
                "damping" => theme.motion.damping = self.number(node, 0.1, 2.0)?,
                other => {
                    return Err(self.error(node, format!("unknown animation option `{other}`")));
                }
            }
        }
        Ok(())
    }

    fn outputs(&self, section: &KdlNode, outputs: &mut Outputs) -> Result<(), ConfigError> {
        for node in self.children(section) {
            match node.name().value() {
                "only" => outputs.only = self.strings(node)?,
                "except" => outputs.except = self.strings(node)?,
                other => return Err(self.error(node, format!("unknown outputs option `{other}`"))),
            }
        }
        Ok(())
    }

    fn sources(&self, section: &KdlNode, sources: &mut Sources) -> Result<(), ConfigError> {
        for node in self.children(section) {
            let slot = match node.name().value() {
                "notifications" => &mut sources.notifications,
                "tray" => &mut sources.tray,
                "media" => &mut sources.media,
                "volume" => &mut sources.volume,
                "brightness" => &mut sources.brightness,
                "power" => &mut sources.power,
                "network" => &mut sources.network,
                "keyboard-layout" => &mut sources.keyboard_layout,
                "screencast" => &mut sources.screencast,
                "microphone" => &mut sources.microphone,
                "camera" => &mut sources.camera,
                "weather" => &mut sources.weather,
                other => return Err(self.error(node, format!("unknown source `{other}`"))),
            };
            *slot = self.flag(node)?;
        }
        Ok(())
    }

    fn timing(&self, section: &KdlNode, timing: &mut Timing) -> Result<(), ConfigError> {
        for node in self.children(section) {
            let slot = match node.name().value() {
                "osd" => &mut timing.osd,
                "status" => &mut timing.status,
                "notification-center" => &mut timing.notification_center,
                other => return Err(self.error(node, format!("unknown timing option `{other}`"))),
            };
            *slot = self.duration(node)?;
        }
        Ok(())
    }

    fn weather(&self, section: &KdlNode, weather: &mut Weather) -> Result<(), ConfigError> {
        for node in self.children(section) {
            match node.name().value() {
                "location" => {
                    let place = self.string(node)?;
                    weather.location = Some(place).filter(|p| !p.trim().is_empty());
                }
                "units" => {
                    weather.units = match self.string(node)?.as_str() {
                        "celsius" => Some(Units::Celsius),
                        "fahrenheit" => Some(Units::Fahrenheit),
                        "auto" => None,
                        other => {
                            return Err(self.error(
                                node,
                                format!("units `{other}`: use celsius, fahrenheit or auto"),
                            ));
                        }
                    }
                }
                "interval" => {
                    let interval = self.duration(node)?;
                    if interval < Duration::from_secs(60) {
                        return Err(self.error(node, "interval: at least 1m (wttr.in is free)"));
                    }
                    weather.interval = interval;
                }
                other => return Err(self.error(node, format!("unknown weather option `{other}`"))),
            }
        }
        Ok(())
    }

    fn session(&self, section: &KdlNode, session: &mut Session) -> Result<(), ConfigError> {
        for node in self.children(section) {
            let slot = match node.name().value() {
                "hold" => {
                    session.hold = self.duration(node)?;
                    continue;
                }
                "poweroff" => &mut session.poweroff,
                "reboot" => &mut session.reboot,
                "logout" => &mut session.logout,
                other => return Err(self.error(node, format!("unknown session option `{other}`"))),
            };
            *slot = self.string(node)?;
        }
        Ok(())
    }
}

/// Parses `#rrggbb` or `#rrggbbaa`.
pub fn parse_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
    if !hex.is_ascii() || !(hex.len() == 6 || hex.len() == 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let a = if hex.len() == 8 { byte(6)? } else { 255 };
    Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_file_matches_defaults() {
        assert_eq!(Config::parse(DEFAULT_CONFIG).unwrap(), Config::default());
    }

    #[test]
    fn empty_config_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn parses_options_in_v2_and_v1_syntax() {
        let v2 = r##"
            theme {
                font "Geist"
                pill-color "#101010"
                accent-colors high="#ffcc00"
                pill-height 28
            }
            clock { format "%I:%M %p"; }
            animation { off; }
            outputs { except "HDMI-A-1"; }
            sources { tray #false; }
            timing { osd "900ms"; }
            session { hold "1s"; logout "loginctl terminate-session self"; }
            weather { location "Saint Petersburg"; units "fahrenheit"; interval "30m"; }
        "##;
        let c = Config::parse(v2).unwrap();
        assert_eq!(c.font.as_deref(), Some("Geist"));
        assert_eq!(c.theme.pill_color, Color::rgba(16, 16, 16, 255));
        assert_eq!(c.theme.priority_colors[2], Color::rgba(255, 204, 0, 255));
        assert_eq!(c.theme.pill_top, 4.0, "re-centered in the strip");
        assert_eq!(c.clock.time, "%I:%M %p");
        assert!(!c.theme.motion.enabled);
        assert!(!c.outputs.wants(Some("HDMI-A-1")) && c.outputs.wants(Some("eDP-1")));
        assert!(!c.sources.tray && c.sources.media);
        assert_eq!(c.timing.osd, Duration::from_millis(900));
        assert_eq!(c.session.hold, Duration::from_secs(1));
        assert_eq!(
            c.session.command(SessionAction::Logout),
            "loginctl terminate-session self"
        );
        assert_eq!(c.session.reboot, "systemctl reboot");
        assert_eq!(c.weather.location.as_deref(), Some("Saint Petersburg"));
        assert_eq!(c.weather.units, Some(Units::Fahrenheit));
        assert_eq!(c.weather.interval, Duration::from_secs(30 * 60));
        assert!(Config::parse("weather { interval \"10s\"; }").is_err());
        assert!(Config::parse("weather { units \"kelvin\"; }").is_err());

        let v1 = "sources {\n    tray false\n}\n";
        assert!(!Config::parse(v1).unwrap().sources.tray);
    }

    #[test]
    fn errors_name_the_problem_and_line() {
        let cases = [
            ("bogus {}", 1, "unknown section `bogus`"),
            (
                "theme {\n  pill-colour \"#000000\"\n}",
                2,
                "unknown theme option `pill-colour`",
            ),
            ("theme {\n  pill-color \"red\"\n}", 2, "invalid color"),
            ("timing {\n\n  osd \"3\"\n}", 3, "needs a unit"),
            ("clock { format \"%Q\"; }", 1, "invalid strftime"),
            ("theme { pill-height 500; }", 1, "between"),
        ];
        for (text, line, needle) in cases {
            let e = Config::parse(text).unwrap_err();
            assert_eq!(e.line, line, "{text}: {e}");
            assert!(e.message.contains(needle), "{text}: {e}");
        }
        assert!(Config::parse("theme {").unwrap_err().line > 0);
    }

    #[test]
    fn outputs_only() {
        let o = Outputs {
            only: vec!["eDP-1".into()],
            except: vec![],
        };
        assert!(o.wants(Some("eDP-1")) && !o.wants(Some("DP-2")) && !o.wants(None));
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff8000"), Some(Color::rgba(255, 128, 0, 255)));
        assert_eq!(
            parse_color("#ff800080"),
            Some(Color::rgba(255, 128, 0, 128))
        );
        assert_eq!(parse_color("ff8000"), None);
        assert_eq!(parse_color("#ff80"), None);
    }
}
