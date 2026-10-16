//! `dmsh render`: draws the islands without a display. Activities are pushed on a
//! simulated clock and animations are stepped at 120 Hz, so any moment of an animation can
//! be rendered deterministically, or a whole stretch as a filmstrip.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use dmsh_core::{Activities, ActivitySpec, IslandKind, Stage, Theme, View};
use dmsh_render::{
    Renderer,
    tiny_skia::{self, Pixmap, PixmapPaint, Transform},
};

const FRAME: Duration = Duration::from_micros(8_333);

pub struct Options {
    /// Theme and font come from here (defaults unless `--config` is given).
    pub config: dmsh_core::config::Config,
    pub output: String,
    pub width: f32,
    pub scale: f32,
    pub activities: Option<String>,
    pub at: Duration,
    pub hover: Option<IslandKind>,
    pub hover_at: Duration,
    pub filmstrip: Option<Duration>,
    pub clock: String,
    pub date: String,
    pub background: Option<String>,
    /// Logical `(x, width)` to keep.
    pub crop: Option<(f32, f32)>,
    /// Pointer position for hover highlights, in logical surface coordinates.
    pub pointer: Option<(f32, f32)>,
    pub battery: Option<dmsh_core::BatteryView>,
    /// Wi-Fi quick control state; `None` hides it.
    pub wifi: Option<bool>,
    /// Do not disturb.
    pub dnd: bool,
}

pub fn render(opts: Options) -> Result<()> {
    let theme = opts.config.theme.clone();
    let mut renderer = Renderer::new(opts.config.font.as_deref());
    let mut pending = load_activities(opts.activities.as_deref())?;
    let background = opts.background.as_deref().map(parse_color).transpose()?;

    let mut images = crate::images::ImageResolver::new();
    for (_, spec) in &mut pending {
        if let Some(image) = spec.image.take() {
            spec.image = images.resolve(&image, &mut renderer);
        }
    }
    let t0 = Instant::now();
    let mut activities = Activities::default();
    activities.set_quiet(opts.dnd);
    let mut stage = Stage::default();
    let mut last: Option<(View, Option<IslandKind>)> = None;
    let mut frames = Vec::new();
    let mut next_frame = Duration::ZERO;
    let mut t = Duration::ZERO;
    loop {
        let now = t0 + t;
        for (_, spec) in pending.extract_if(.., |(at, _)| *at <= t) {
            activities.push(spec, now).map_err(anyhow::Error::msg)?;
        }
        activities.expire(now);
        let view = View {
            clock: Some(opts.clock.clone()),
            date: Some(opts.date.clone()),
            battery: opts.battery,
            controls: dmsh_core::Controls {
                wifi: opts.wifi,
                keep_awake: false,
                dnd: opts.dnd,
            },
            ..activities.view(now)
        };
        let hover = opts.hover.filter(|_| t >= opts.hover_at);
        if last.as_ref() != Some(&(view.clone(), hover)) {
            stage.update(opts.width, &view, hover, &theme, &mut renderer);
            last = Some((view, hover));
        }
        stage.set_pointer(opts.pointer);
        if let Some(interval) = opts.filmstrip
            && t >= next_frame
        {
            frames.push(draw(&stage, &theme, &mut renderer, &opts, background)?);
            next_frame += interval;
        }
        if t >= opts.at {
            break;
        }
        stage.step(FRAME.as_secs_f32());
        t += FRAME;
    }
    if opts.filmstrip.is_none() {
        frames.push(draw(&stage, &theme, &mut renderer, &opts, background)?);
    }

    let image = stack(&frames)?;
    image
        .save_png(&opts.output)
        .with_context(|| format!("writing {}", opts.output))?;
    println!(
        "{}: {}x{} ({} frame{})",
        opts.output,
        image.width(),
        image.height(),
        frames.len(),
        if frames.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

/// Activity specs, each optionally delayed by an `at_ms` field.
fn load_activities(path: Option<&str>) -> Result<Vec<(Duration, ActivitySpec)>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let json = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&json).context("parsing activities")?;
    entries
        .into_iter()
        .map(|mut entry| {
            let at = entry
                .as_object_mut()
                .and_then(|o| o.remove("at_ms"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let spec = serde_json::from_value(entry).context("parsing activity")?;
            Ok((Duration::from_millis(at), spec))
        })
        .collect()
}

fn draw(
    stage: &Stage,
    theme: &Theme,
    renderer: &mut Renderer,
    opts: &Options,
    background: Option<tiny_skia::Color>,
) -> Result<Pixmap> {
    let scene = stage.scene(theme);
    let bw = (opts.width * opts.scale).round() as u32;
    let bh = (stage.canvas_height(theme) as f32 * opts.scale).round() as u32;
    let mut layer = Pixmap::new(bw, bh).context("invalid size")?;
    renderer.render(&scene, opts.scale, &mut layer.as_mut());

    let (x, w) = match opts.crop {
        Some((x, w)) => (
            (x * opts.scale).round() as i32,
            (w * opts.scale).round() as u32,
        ),
        None => (0, bw),
    };
    let mut frame = Pixmap::new(w, bh).context("invalid crop")?;
    if let Some(bg) = background {
        frame.fill(bg);
    }
    frame.draw_pixmap(
        -x,
        0,
        layer.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    Ok(frame)
}

/// Stacks frames vertically with a thin separator.
fn stack(frames: &[Pixmap]) -> Result<Pixmap> {
    if let [only] = frames {
        return Ok(only.clone());
    }
    let gap = 3;
    let w = frames
        .iter()
        .map(Pixmap::width)
        .max()
        .context("no frames")?;
    let row = frames.iter().map(Pixmap::height).max().unwrap_or(1);
    let h = frames.len() as u32 * (row + gap);
    let mut out = Pixmap::new(w, h).context("filmstrip too large")?;
    out.fill(tiny_skia::Color::from_rgba8(255, 0, 255, 255));
    for (i, f) in frames.iter().enumerate() {
        let y = (i as u32 * (row + gap)) as i32;
        let mut cell = Pixmap::new(w, row).context("invalid size")?;
        cell.fill(tiny_skia::Color::from_rgba8(60, 60, 64, 255));
        cell.draw_pixmap(
            0,
            0,
            f.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        out.draw_pixmap(
            0,
            y,
            cell.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
    Ok(out)
}

/// Parses `#rrggbb`.
fn parse_color(s: &str) -> Result<tiny_skia::Color> {
    let hex = s.trim_start_matches('#');
    if hex.len() != 6 {
        bail!("colors look like #rrggbb");
    }
    let n = u32::from_str_radix(hex, 16).context("invalid color")?;
    Ok(tiny_skia::Color::from_rgba8(
        (n >> 16) as u8,
        (n >> 8) as u8,
        n as u8,
        255,
    ))
}
