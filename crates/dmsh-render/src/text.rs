//! Text shaping and drawing with cosmic-text. Fonts come from fontconfig: the UI family
//! (Inter if installed, else the system sans-serif) plus a color emoji font, nothing else,
//! so startup stays fast and memory small. Shaped buffers are cached per string, size and
//! scale, so steady animation frames only blend cached glyphs.

use std::collections::HashMap;
use std::process::Command;

use cosmic_text::{
    Attrs, Buffer, Color as TextColor, Ellipsize, EllipsizeHeightLimit, Family, FontSystem,
    Metrics, Shaping, SwashCache, Weight as FontWeight, Wrap, fontdb,
};
use dmsh_core::{Align, TextDraw, TextStyle, Weight};
use tiny_skia::PixmapMut;

/// Preferred UI families, best first; the last resort is whatever fontconfig picks.
const FAMILIES: &[&str] = &["Inter", "SF Pro Text"];
/// Shaped buffers kept before the cache is reset.
const CACHE_LIMIT: usize = 512;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    size: u32,
    weight: Weight,
    max_width: u32,
    max_lines: usize,
}

pub struct TextEngine {
    fonts: FontSystem,
    glyphs: SwashCache,
    family: String,
    buffers: HashMap<Key, Buffer>,
}

/// Pixel clip in buffer coordinates: `x0..x1`, `y0..y1`.
#[derive(Clone, Copy)]
pub struct Clip {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl TextEngine {
    /// `font` is tried first; then Inter, SF Pro Text, the system sans-serif.
    pub fn new(font: Option<&str>) -> Self {
        let (db, family) = load_fonts(font);
        log::debug!("UI font: {family} ({} faces)", db.len());
        Self {
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            glyphs: SwashCache::new(),
            family,
            buffers: HashMap::new(),
        }
    }

    /// Lays out `text` at `scale` into the cache and returns its key.
    fn shape(
        &mut self,
        text: &str,
        style: TextStyle,
        max_width: f32,
        max_lines: usize,
        scale: f32,
    ) -> Key {
        let key = Key {
            text: text.to_owned(),
            size: (style.size * scale).to_bits(),
            weight: style.weight,
            max_width: (max_width * scale).to_bits(),
            max_lines,
        };
        if self.buffers.len() >= CACHE_LIMIT && !self.buffers.contains_key(&key) {
            self.buffers.clear();
        }
        let Self {
            fonts,
            family,
            buffers,
            ..
        } = self;
        buffers.entry(key.clone()).or_insert_with(|| {
            let metrics = Metrics::new(style.size * scale, style.line_height() * scale);
            let mut buffer = Buffer::new(fonts, metrics);
            buffer.set_wrap(if max_lines > 1 {
                Wrap::Word
            } else {
                Wrap::None
            });
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(max_lines)));
            buffer.set_size(Some(max_width * scale), None);
            let attrs = Attrs::new()
                .family(Family::Name(family))
                .weight(font_weight(style.weight));
            buffer.set_text(text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(fonts, false);
            buffer
        });
        key
    }

    pub fn measure(
        &mut self,
        text: &str,
        style: TextStyle,
        max_width: f32,
        max_lines: usize,
    ) -> (f32, f32) {
        let key = self.shape(text, style, max_width, max_lines, 1.0);
        let buffer = &self.buffers[&key];
        let (mut width, mut lines) = (0.0_f32, 0);
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        (width.ceil(), lines.max(1) as f32 * style.line_height())
    }

    pub fn draw(
        &mut self,
        t: &TextDraw,
        scale: f32,
        alpha: f32,
        clip: Option<Clip>,
        pixmap: &mut PixmapMut,
    ) {
        let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
        let clip = clip.unwrap_or(Clip {
            x0: 0,
            y0: 0,
            x1: pw,
            y1: ph,
        });
        let clip = Clip {
            x0: clip.x0.max(0),
            y0: clip.y0.max(0),
            x1: clip.x1.min(pw),
            y1: clip.y1.min(ph),
        };
        let mut ox = (t.rect.x * scale).round() as i32;
        let oy = (t.rect.y * scale).round() as i32;

        let color = TextColor::rgba(t.color.r, t.color.g, t.color.b, t.color.a);
        let max_width = t.rect.w * scale;
        let key = self.shape(&t.text, t.style, t.rect.w, t.max_lines, scale);
        let Self {
            fonts,
            glyphs,
            buffers,
            ..
        } = self;
        let buffer = buffers.get_mut(&key).expect("just shaped");
        if t.align == Align::Center {
            let line_w = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
            ox += ((max_width - line_w) / 2.0).round() as i32;
        }
        let data = pixmap.data_mut();
        buffer.draw(fonts, glyphs, color, |x, y, w, h, c| {
            let a = c.a() as f32 / 255.0 * alpha;
            if a <= 0.0 {
                return;
            }
            for py in oy + y..oy + y + h as i32 {
                if py < clip.y0 || py >= clip.y1 {
                    continue;
                }
                for px in ox + x..ox + x + w as i32 {
                    if px < clip.x0 || px >= clip.x1 {
                        continue;
                    }
                    let i = (py * pw + px) as usize * 4;
                    blend(&mut data[i..i + 4], [c.r(), c.g(), c.b()], a);
                }
            }
        });
    }
}

/// Source-over of a straight-alpha color onto a premultiplied RGBA pixel.
fn blend(dst: &mut [u8], rgb: [u8; 3], a: f32) {
    let inv = 1.0 - a;
    for (d, s) in dst[..3].iter_mut().zip(rgb) {
        *d = (s as f32 * a + *d as f32 * inv).round() as u8;
    }
    dst[3] = (255.0 * a + dst[3] as f32 * inv).round() as u8;
}

fn font_weight(w: Weight) -> FontWeight {
    match w {
        Weight::Regular => FontWeight::NORMAL,
        Weight::Medium => FontWeight::MEDIUM,
        Weight::Semibold => FontWeight::SEMIBOLD,
        Weight::Bold => FontWeight::BOLD,
    }
}

fn load_fonts(preferred: Option<&str>) -> (fontdb::Database, String) {
    let mut db = fontdb::Database::new();
    let family = preferred
        .into_iter()
        .chain(FAMILIES.iter().copied())
        .find(|f| fc_match(f, "%{family[0]}").is_some_and(|m| m.eq_ignore_ascii_case(f)))
        .map(|f| f.to_string())
        .or_else(|| fc_match("sans-serif", "%{family[0]}"))
        .unwrap_or_else(|| "sans-serif".into());

    for file in fc_list_files(&family) {
        let _ = db.load_font_file(file);
    }
    if let Some(emoji) = fc_match("emoji", "%{file}") {
        let _ = db.load_font_file(emoji);
    }
    if db.is_empty() {
        log::warn!("fontconfig found no fonts; scanning all system fonts");
        db.load_system_fonts();
    }
    db.set_sans_serif_family(family.clone());
    (db, family)
}

fn fc_match(pattern: &str, format: &str) -> Option<String> {
    let out = Command::new("fc-match")
        .args(["-f", format, pattern])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    (!s.is_empty()).then_some(s)
}

fn fc_list_files(family: &str) -> Vec<String> {
    Command::new("fc-list")
        .args(["-f", "%{file}\n", family])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_is_source_over() {
        let mut px = [0, 0, 0, 0];
        blend(&mut px, [255, 255, 255], 1.0);
        assert_eq!(px, [255, 255, 255, 255]);
        let mut px = [0, 0, 0, 255];
        blend(&mut px, [255, 0, 0], 0.5);
        assert_eq!(px, [128, 0, 0, 255]);
    }
}
