mod shell;

use anyhow::{Context, Result, bail};
use dmsh_core::{Layout, Theme};

const USAGE: &str = "\
usage:
  dmsh                     run the shell
  dmsh render [options]    draw the islands headlessly to a PNG
      -o, --output <path>  output file (default: dmsh.png)
      --width <px>         logical output width (default: 1645)
      --scale <factor>     output scale (default: 1.75)";

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => shell::run(),
        Some("render") => render(&args[1..]),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}

fn render(args: &[String]) -> Result<()> {
    let mut output = String::from("dmsh.png");
    let mut width = 1645.0_f32;
    let mut scale = 1.75_f32;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-o" | "--output" => output = value()?.clone(),
            "--width" => width = value()?.parse().context("--width")?,
            "--scale" => scale = value()?.parse().context("--scale")?,
            _ => bail!("unknown option `{arg}`\n\n{USAGE}"),
        }
    }

    let theme = Theme::default();
    let scene = Layout::compute(width, &theme).scene(&theme);
    let (bw, bh) = (
        (width * scale).round() as u32,
        (theme.surface_height as f32 * scale).round() as u32,
    );
    let mut pixmap = dmsh_render::tiny_skia::Pixmap::new(bw, bh).context("invalid size")?;
    dmsh_render::render(&scene, scale, &mut pixmap.as_mut());
    pixmap
        .save_png(&output)
        .with_context(|| format!("writing {output}"))?;
    println!("{output}: {bw}x{bh}");
    Ok(())
}
