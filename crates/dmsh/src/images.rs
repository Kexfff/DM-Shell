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
        let path = image.strip_prefix("file://").unwrap_or(image);
        let (path, icon) = if path.starts_with('/') {
            // Theme and pixmap files are artwork; anything else (an avatar) is a photo.
            let icon = path.contains("/icons/") || path.contains("/pixmaps/");
            (Path::new(path).to_path_buf(), icon)
        } else {
            (self.icons.find(path, None)?, true)
        };
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if !path.is_file() || !matches!(ext.as_str(), "png" | "svg") {
            log::debug!("unusable image {}", path.display());
            return None;
        }
        let key = format!("file:{}", path.display());
        renderer.set_image(&key, ImageSource::File(path), icon);
        Some(key)
    }
}
