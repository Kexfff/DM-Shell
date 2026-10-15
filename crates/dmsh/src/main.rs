mod headless;
mod ipc;
mod shell;

use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use dmsh_core::{
    ActivitySpec, IslandKind, Placement, Priority,
    ipc::{Reply, Request, Response},
};

/// A minimal dynamic-island shell for niri.
///
/// Run without a command to start the shell. Other commands talk to the running shell
/// over its socket ($DMSH_SOCKET, else $XDG_RUNTIME_DIR/dmsh-$WAYLAND_DISPLAY.sock).
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the shell (the default).
    Run,
    /// Create, update or remove activities.
    #[command(subcommand)]
    Activity(ActivityCommand),
    /// Print what the islands show and every live activity, as JSON.
    State,
    /// Send one raw JSON request line and print the raw response.
    Msg {
        /// e.g. '{"cmd":"push","id":"x","title":"Hello"}'
        json: String,
    },
    /// Print the JSON Schema of the IPC protocol.
    Schema,
    /// List the built-in icon names.
    Icons,
    /// Draw the islands headlessly to a PNG, simulating animations on a fake clock.
    Render {
        #[arg(short, long, default_value = "dmsh.png")]
        output: String,
        /// Logical output width.
        #[arg(long, default_value_t = 1645.0)]
        width: f32,
        #[arg(long, default_value_t = 1.75)]
        scale: f32,
        /// JSON file with an array of activity specs (as sent by `push`); an `at_ms` field
        /// delays an entry.
        #[arg(long)]
        activities: Option<String>,
        /// Render this long into the simulation, e.g. 2500ms.
        #[arg(long, value_parser = parse_duration, default_value = "3s")]
        at: Duration,
        /// Hover an island (center or right).
        #[arg(long, value_parser = parse_hover)]
        hover: Option<IslandKind>,
        /// When the hover starts.
        #[arg(long, value_parser = parse_duration, default_value = "0ms")]
        hover_at: Duration,
        /// Render a frame every interval up to --at, stacked vertically.
        #[arg(long, value_parser = parse_duration)]
        filmstrip: Option<Duration>,
        #[arg(long, default_value = "13:35")]
        clock: String,
        #[arg(long, default_value = "Thursday, 15 October")]
        date: String,
        /// Background color behind the islands, e.g. #3c3c40 (default: transparent).
        #[arg(long)]
        background: Option<String>,
        /// Keep only a logical x range: <x>,<width>.
        #[arg(long, value_parser = parse_crop)]
        crop: Option<(f32, f32)>,
    },
}

#[derive(Subcommand)]
enum ActivityCommand {
    /// Create or update an activity (same id = update).
    Push {
        #[arg(long)]
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: Option<String>,
        /// 0.0 to 1.0.
        #[arg(long)]
        progress: Option<f32>,
        /// Built-in icon name (see `dmsh icons`).
        #[arg(long)]
        icon: Option<String>,
        /// center, right, or flow (center briefly, then right).
        #[arg(long, value_parser = parse_enum::<Placement>, default_value = "flow")]
        placement: Placement,
        /// low, normal, high or critical.
        #[arg(long, value_parser = parse_enum::<Priority>, default_value = "normal")]
        priority: Priority,
        /// Remove this long after the last push, e.g. 1500ms, 30s, 5m. Default: until dismissed.
        #[arg(long, value_parser = parse_duration)]
        ttl: Option<Duration>,
        /// How long a flow activity stays in the center.
        #[arg(long, value_parser = parse_duration, default_value = "2s")]
        center_for: Duration,
    },
    /// Remove an activity.
    Dismiss { id: String },
    /// Remove all activities.
    Clear,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,zbus=warn,tracing=warn"),
    )
    .init();

    match Cli::parse().command.unwrap_or(Command::Run) {
        Command::Run => shell::run(),
        Command::Activity(cmd) => activity(cmd),
        Command::State => {
            let Reply::State(state) = call(&Request::State)? else {
                bail!("unexpected reply to state");
            };
            println!("{}", serde_json::to_string_pretty(&state)?);
            Ok(())
        }
        Command::Msg { json } => {
            println!("{}", ipc::request_raw(&json)?);
            Ok(())
        }
        Command::Schema => {
            let schema = dmsh_core::ipc::schema();
            println!("{}", serde_json::to_string_pretty(&schema)?);
            Ok(())
        }
        Command::Icons => {
            dmsh_render::icon_names().for_each(|name| println!("{name}"));
            Ok(())
        }
        Command::Render {
            output,
            width,
            scale,
            activities,
            at,
            hover,
            hover_at,
            filmstrip,
            clock,
            date,
            background,
            crop,
        } => headless::render(headless::Options {
            output,
            width,
            scale,
            activities,
            at,
            hover,
            hover_at,
            filmstrip,
            clock,
            date,
            background,
            crop,
        }),
    }
}

fn activity(cmd: ActivityCommand) -> Result<()> {
    let request = match cmd {
        ActivityCommand::Push {
            id,
            title,
            body,
            progress,
            icon,
            placement,
            priority,
            ttl,
            center_for,
        } => Request::Push(ActivitySpec {
            body,
            progress,
            icon,
            placement,
            priority,
            ttl_ms: ttl.map(|d| d.as_millis() as u64),
            center_ms: center_for.as_millis() as u64,
            ..ActivitySpec::new(id, title)
        }),
        ActivityCommand::Dismiss { id } => Request::Dismiss { id },
        ActivityCommand::Clear => Request::Clear,
    };
    if let Reply::Dismissed { existed: false } = call(&request)? {
        bail!("no such activity");
    }
    Ok(())
}

/// Sends a request and turns an error response into an error.
fn call(request: &Request) -> Result<Reply> {
    match ipc::request(request)? {
        Response::Ok(reply) => Ok(reply),
        Response::Error(e) => bail!("{e}"),
    }
}

/// Parses `1500ms`, `2s` or `5m`.
fn parse_duration(s: &str) -> Result<Duration, String> {
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

fn parse_hover(s: &str) -> Result<IslandKind, String> {
    match s {
        "center" => Ok(IslandKind::Center),
        "right" => Ok(IslandKind::Right),
        _ => Err("expected center or right".into()),
    }
}

/// Parses `<x>,<width>`.
fn parse_crop(s: &str) -> Result<(f32, f32), String> {
    let (x, w) = s.split_once(',').ok_or("expected <x>,<width>")?;
    let parse = |v: &str| v.trim().parse::<f32>().map_err(|e| e.to_string());
    Ok((parse(x)?, parse(w)?))
}

/// Parses an enum by its JSON name, so the CLI and the protocol accept the same values.
fn parse_enum<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1500ms"), Ok(Duration::from_millis(1500)));
        assert_eq!(parse_duration("2s"), Ok(Duration::from_secs(2)));
        assert_eq!(parse_duration("0.5m"), Ok(Duration::from_secs(30)));
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("3h").is_err());
    }

    #[test]
    fn enums_match_protocol_names() {
        assert_eq!(parse_enum::<Placement>("right"), Ok(Placement::Right));
        assert!(parse_enum::<Priority>("urgent").is_err());
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
