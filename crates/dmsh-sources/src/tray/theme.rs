//! Minimal freedesktop icon theme lookup: the user's theme (GTK settings), its `Inherits`
//! chain, then hicolor and `/usr/share/pixmaps`. Theme indexes and results are cached, so
//! each icon name costs a handful of `stat`s once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Tray icons are drawn around 16–28 px; prefer directories near this size.
const WANTED_SIZE: i64 = 32;

pub struct IconFinder {
    theme: String,
    bases: Vec<PathBuf>,
    indexes: HashMap<String, Option<Index>>,
    found: HashMap<(String, Option<String>), Option<PathBuf>>,
}

struct Index {
    /// Subdirectories, best size first.
    dirs: Vec<String>,
    inherits: Vec<String>,
}

impl IconFinder {
    pub fn new() -> Self {
        Self {
            theme: user_theme().unwrap_or_else(|| "hicolor".into()),
            bases: base_dirs(),
            indexes: HashMap::new(),
            found: HashMap::new(),
        }
    }

    /// Finds an icon file by name, also searching an app-provided `extra` directory.
    pub fn find(&mut self, name: &str, extra: Option<&str>) -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        let key = (name.to_owned(), extra.map(str::to_owned));
        if let Some(found) = self.found.get(&key) {
            return found.clone();
        }
        let found = self.lookup(name, extra);
        self.found.insert(key, found.clone());
        found
    }

    fn lookup(&mut self, name: &str, extra: Option<&str>) -> Option<PathBuf> {
        if name.starts_with('/') {
            return Path::new(name).exists().then(|| name.into());
        }
        if let Some(dir) = extra.filter(|d| !d.is_empty()) {
            let dir = Path::new(dir);
            if let Some(file) = with_extension(&dir.join(name)) {
                return Some(file);
            }
            // App directories are often themes themselves (`<dir>/hicolor/22x22/apps`).
            for theme in ["hicolor", ""] {
                if let Some(file) = self.search_theme_in(dir, theme, name) {
                    return Some(file);
                }
            }
        }
        let mut queue = vec![self.theme.clone()];
        let mut seen = Vec::new();
        while let Some(theme) = queue.pop() {
            if seen.contains(&theme) {
                continue;
            }
            seen.push(theme.clone());
            if let Some(file) = self.search_theme(&theme, name) {
                return Some(file);
            }
            if let Some(index) = self.index(&theme) {
                queue.extend(index.inherits.iter().rev().cloned());
            }
            if queue.is_empty() && !seen.iter().any(|t| t == "hicolor") {
                queue.push("hicolor".into());
            }
        }
        with_extension(&Path::new("/usr/share/pixmaps").join(name))
    }

    fn search_theme(&mut self, theme: &str, name: &str) -> Option<PathBuf> {
        let dirs = self.index(theme)?.dirs.clone();
        self.bases.iter().find_map(|base| {
            dirs.iter()
                .find_map(|dir| with_extension(&base.join(theme).join(dir).join(name)))
        })
    }

    fn search_theme_in(&mut self, base: &Path, theme: &str, name: &str) -> Option<PathBuf> {
        let dirs = self
            .index(if theme.is_empty() { "hicolor" } else { theme })?
            .dirs
            .clone();
        dirs.iter()
            .find_map(|dir| with_extension(&base.join(theme).join(dir).join(name)))
    }

    fn index(&mut self, theme: &str) -> Option<&Index> {
        if !self.indexes.contains_key(theme) {
            let index = self
                .bases
                .iter()
                .find_map(|b| std::fs::read_to_string(b.join(theme).join("index.theme")).ok())
                .map(|text| parse_index(&text));
            self.indexes.insert(theme.to_owned(), index);
        }
        self.indexes.get(theme)?.as_ref()
    }
}

fn with_extension(stem: &Path) -> Option<PathBuf> {
    ["svg", "png"]
        .iter()
        .map(|ext| stem.with_extension(ext))
        .find(|p| p.exists())
}

/// Parses `index.theme`: the directory list (sorted by closeness to [`WANTED_SIZE`],
/// scalable ones first among equals) and `Inherits`.
fn parse_index(text: &str) -> Index {
    let mut directories = Vec::new();
    let mut inherits = Vec::new();
    let mut sizes: HashMap<String, (i64, bool)> = HashMap::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_owned();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if section == "Icon Theme" {
            let list = || {
                value
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
            };
            match key {
                "Directories" | "ScaledDirectories" => directories.extend(list()),
                "Inherits" => inherits.extend(list()),
                _ => {}
            }
        } else {
            let entry = sizes.entry(section.clone()).or_insert((0, false));
            match key {
                "Size" => entry.0 = value.parse().unwrap_or(0),
                "Type" => entry.1 = value.eq_ignore_ascii_case("scalable"),
                _ => {}
            }
        }
    }
    directories.dedup();
    directories.sort_by_key(|d| {
        let (size, scalable) = sizes.get(d).copied().unwrap_or((0, false));
        ((size - WANTED_SIZE).abs(), !scalable)
    });
    Index {
        dirs: directories,
        inherits,
    }
}

fn base_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    if let Some(h) = &home {
        dirs.push(h.join(".icons"));
    }
    dirs.extend(data_home.map(|d| d.join("icons")));
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| Path::new(d).join("icons")),
    );
    dirs
}

/// The icon theme from GTK settings (what most of the desktop already uses).
fn user_theme() -> Option<String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    ["gtk-4.0", "gtk-3.0"].iter().find_map(|v| {
        let text = std::fs::read_to_string(config.join(v).join("settings.ini")).ok()?;
        text.lines().find_map(|l| {
            let value = l.trim().strip_prefix("gtk-icon-theme-name")?;
            let value = value
                .trim_start()
                .strip_prefix('=')?
                .trim()
                .trim_matches('"');
            (!value.is_empty()).then(|| value.to_owned())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_sorts_directories_by_size() {
        let index = parse_index(
            "[Icon Theme]\nName=T\nInherits=breeze,hicolor\nDirectories=16x16/apps,scalable/apps,32x32/apps\n\n\
             [16x16/apps]\nSize=16\n\n[scalable/apps]\nSize=32\nType=Scalable\n\n[32x32/apps]\nSize=32\n",
        );
        assert_eq!(index.dirs, ["scalable/apps", "32x32/apps", "16x16/apps"]);
        assert_eq!(index.inherits, ["breeze", "hicolor"]);
    }
}
