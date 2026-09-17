//! A small reusable widget for a cover-art slot that starts as a plain placeholder and swaps to
//! the real image once one is cached locally (`abs_core::covers::fetch_and_cache_cover`). Not a
//! libadwaita compatibility shim like this module's other widgets — just a graceful-degradation
//! wrapper: a missing or corrupt cached file must never be treated as an error, only as "show the
//! placeholder", since cover art is cosmetic and this codebase already shows a plain placeholder
//! card everywhere covers aren't available yet.

use std::path::Path;

use adw::prelude::*;

#[derive(Clone)]
pub struct CoverImage {
    overlay: gtk4::Overlay,
    placeholder: gtk4::Box,
    picture: gtk4::Picture,
}

impl CoverImage {
    /// `size` is both width and height — every cover slot in this app is square.
    pub fn new(size: i32) -> Self {
        let placeholder = gtk4::Box::builder().css_classes(["card"]).width_request(size).height_request(size).build();
        let picture = gtk4::Picture::builder().width_request(size).height_request(size).content_fit(gtk4::ContentFit::Cover).visible(false).build();

        let overlay = gtk4::Overlay::builder().child(&placeholder).build();
        overlay.add_overlay(&picture);

        Self { overlay, placeholder, picture }
    }

    pub fn widget(&self) -> &gtk4::Widget {
        self.overlay.upcast_ref()
    }

    /// Shows the cached image at `path`, or falls back to the placeholder if `path` is `None` or
    /// the file can't be decoded (missing, corrupt, unsupported format — none of these should
    /// ever crash the player screen over cosmetic art).
    pub fn set_path(&self, path: Option<&Path>) {
        let Some(path) = path else {
            self.show_placeholder();
            return;
        };
        match gtk4::gdk::Texture::from_filename(path) {
            Ok(texture) => {
                self.picture.set_paintable(Some(&texture));
                self.picture.set_visible(true);
                self.placeholder.set_visible(false);
            }
            Err(err) => {
                tracing::warn!(%err, ?path, "couldn't decode the cached cover image");
                self.show_placeholder();
            }
        }
    }

    fn show_placeholder(&self) {
        self.picture.set_visible(false);
        self.placeholder.set_visible(true);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run() {
        let cover = CoverImage::new(64);
        assert!(cover.placeholder.is_visible(), "starts showing the placeholder");
        assert!(!cover.picture.is_visible());

        cover.set_path(None);
        assert!(cover.placeholder.is_visible(), "no path keeps the placeholder");

        let tmp = tempfile::tempdir().unwrap();
        let corrupt = tmp.path().join("not-an-image.png");
        std::fs::write(&corrupt, b"not actually a png").unwrap();
        cover.set_path(Some(&corrupt));
        assert!(cover.placeholder.is_visible(), "a corrupt file should fall back to the placeholder, not panic");
        assert!(!cover.picture.is_visible());

        let valid = tmp.path().join("real.png");
        write_1x1_png(&valid);
        cover.set_path(Some(&valid));
        assert!(cover.picture.is_visible(), "a valid image should show the picture");
        assert!(!cover.placeholder.is_visible());

        cover.set_path(None);
        assert!(cover.placeholder.is_visible(), "clearing the path restores the placeholder");
    }

    /// The smallest possible valid PNG (1x1, black pixel) — enough for `gdk::Texture::from_filename`
    /// to succeed without needing a real asset file in the repo.
    fn write_1x1_png(path: &std::path::Path) {
        const PNG_1X1: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63,
            0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
            0x42, 0x60, 0x82,
        ];
        std::fs::write(path, PNG_1X1).unwrap();
    }
}
