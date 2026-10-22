//! Turns an activity's `image` (a file path, a `file://` URI, an icon-theme name, or pixels
//! a source registered) into a renderer image key.

use std::path::Path;

use dmsh_render::{ImageSource, Renderer};
use dmsh_sources::tray::theme::IconFinder;

pub struct ImageResolver {
    icons: IconFinder,
}

impl ImageResolver {
    pub fn new() -> Self {
        Self {
            icons: IconFinder::new(),
        }
    }

    /// Registers the image with the renderer and returns its key, or `None` if it can't be
    /// used (missing file, unknown icon, unsupported format) so the activity's icon shows.
    pub fn resolve(&mut self, image: &str, renderer: &mut Renderer) -> Option<String> {
        if image.starts_with("data:") {
            return renderer.has_image(image).then(|| image.to_owned());
        }
        let decoded;
        let path = match image.strip_prefix("file://") {
            Some(uri) => {
                decoded = percent_decode(uri);
                decoded.as_str()
            }
            None => image,
        };
        let (path, icon) = if path.starts_with('/') {
            // Theme and pixmap files are artwork; anything else (an avatar) is a photo.
            let icon = path.contains("/icons/") || path.contains("/pixmaps/");
            (Path::new(path).to_path_buf(), icon)
        } else {
            (self.icons.find(path, None)?, true)
        };
        // Browsers' album art has no extension; the renderer goes by content.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !path.is_file() || !matches!(ext.as_str(), "png" | "svg" | "jpg" | "jpeg" | "") {
            log::debug!("unusable image {}", path.display());
            return None;
        }
        let key = format!("file:{}", path.display());
        renderer.set_image(&key, ImageSource::File(path), icon);
        Some(key)
    }
}

/// Decodes `%XX` escapes in a `file://` URI's path (`My%20Music` → `My Music`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        assert_eq!(
            percent_decode("/home/me/My%20Music/a.jpg"),
            "/home/me/My Music/a.jpg"
        );
        assert_eq!(percent_decode("/%D0%BC%D1%83%D0%B7"), "/муз");
        assert_eq!(percent_decode("/100%"), "/100%");
    }
}
