//! A small reusable widget for a cover-art slot that starts as a plain placeholder and swaps to
//! the real image once one is cached locally (`abs_core::covers::fetch_and_cache_cover`). Not a
//! libadwaita compatibility shim like this module's other widgets — just a graceful-degradation
//! wrapper: a missing or corrupt cached file must never be treated as an error, only as "show the
//! placeholder", since cover art is cosmetic and this codebase already shows a plain placeholder
//! card everywhere covers aren't available yet.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;

#[derive(Clone)]
pub struct CoverImage {
    overlay: gtk4::Overlay,
    placeholder: gtk4::Box,
    picture: gtk4::Picture,
    /// The path whose image is currently rendered, shared across clones (the widget handle and
    /// the closure it was cloned into must agree). Snapshots arrive ~4×/second while anything
    /// is playing — the player's 250 ms tick republishes to advance the scrubber — and every
    /// one re-runs `set_path`; `set_path` decodes from disk, so an unchanged path has to
    /// short-circuit rather than re-decode the same file four times a second.
    last_path: Rc<Cell<Option<PathBuf>>>,
}

impl CoverImage {
    /// `size` is both width and height — every cover slot in this app is square.
    pub fn new(size: i32) -> Self {
        // Every widget here is pinned to a fixed, non-expanding, non-stretching `size`x`size` box.
        // The sizing discipline below was learned the hard way once real covers actually started
        // loading (WebP took until the in-process decoder landed; see `set_path`):
        // 1. `GtkPicture` defaults to `hexpand`/`vexpand: true`, and with only one sibling in a
        //    shelf row (as Home's "Continue Listening" often has), nothing else claimed the
        //    leftover space, so the whole card stretched into a short, wide rectangle instead of
        //    staying square — `content-fit: Cover` then cropped the real cover into that
        //    wrong-aspect box, visibly distorting it.
        // 2. Fixing that (explicit `hexpand`/`vexpand: false`) still left the *overlay* itself at
        //    its default `halign`/`valign: Fill` — so it stayed square, but a single-item row
        //    could still allocate the whole card more width than its `size`x`size` natural size,
        //    and Fill alignment stretched the overlay (and the picture inside it) to match,
        //    rendering a *correctly square but uniformly larger* cover than the same widget gets
        //    in a row with enough siblings to fill the space (confirmed live, side by side: the
        //    same "Continue Listening" cover rendered visibly more zoomed-in than the identical
        //    item's card in "Recently Added"). `halign`/`valign: Center` on the overlay pins it to
        //    exactly its natural size regardless of how much extra space a parent offers it.
        let placeholder = gtk4::Box::builder().css_classes(["card"]).width_request(size).height_request(size).build();
        let picture = gtk4::Picture::builder()
            .width_request(size)
            .height_request(size)
            .content_fit(gtk4::ContentFit::Cover)
            .hexpand(false)
            .vexpand(false)
            .halign(gtk4::Align::Fill)
            .valign(gtk4::Align::Fill)
            .visible(false)
            .build();

        let overlay = gtk4::Overlay::builder()
            .child(&placeholder)
            .width_request(size)
            .height_request(size)
            .hexpand(false)
            .vexpand(false)
            .halign(gtk4::Align::Center)
            .valign(gtk4::Align::Center)
            .build();
        overlay.add_overlay(&picture);

        Self { overlay, placeholder, picture, last_path: Rc::new(Cell::new(None)) }
    }

    pub fn widget(&self) -> &gtk4::Widget {
        self.overlay.upcast_ref()
    }

    /// Test-only view of the underlying `GtkPicture` — scenarios assert visibility on it to pin,
    /// end to end, that a cached cover actually rendered rather than stayed a placeholder.
    #[cfg(test)]
    pub(crate) fn picture(&self) -> &gtk4::Picture {
        &self.picture
    }

    /// Shows the cached image at `path`, or falls back to the placeholder if `path` is `None` or
    /// the file can't be decoded (missing, corrupt, unsupported format — none of these should
    /// ever crash the player screen over cosmetic art). Repeated calls with the same path are a
    /// no-op: the snapshot cadence (~4 Hz) would otherwise re-decode the same file from disk on
    /// every tick.
    pub fn set_path(&self, path: Option<&Path>) {
        // `replace` stores the new value and hands back the previous one in a single step — the
        // comparison can't precede the store, or a rapid A→A sequence (same path republished)
        // would re-decode. Note a path whose *contents* changed under the same name therefore
        // won't repaint until some other path lands; covers are cached item-keyed, so that
        // only happens when a server replaces a cover in place, and the next session's decode
        // picks the new bytes up.
        let previous = self.last_path.replace(path.map(Path::to_path_buf));
        if previous.as_deref() == path {
            return;
        }
        let Some(path) = path else {
            self.show_placeholder();
            return;
        };
        // Fast path: gdk-pixbuf (via `Texture::from_filename`). Covers only the formats the
        // host's pixbuf loaders support. Fallback path: decode in-process with the `image`
        // crate — needed for WebP, which gdk-pixbuf has no loader for on the target distros
        // (no WebP loader ships in Debian bookworm, PureOS Crimson's base, and Audiobookshelf
        // serves plenty of WebP covers).
        match gtk4::gdk::Texture::from_filename(path) {
            Ok(texture) => {
                self.show_texture(texture);
            }
            Err(pixbuf_err) => match decode_texture_in_process(path) {
                Ok(texture) => {
                    self.show_texture(texture);
                }
                Err(err) => {
                    tracing::warn!(%err, ?path, "couldn't decode the cached cover image");
                    self.show_placeholder();
                    let _ = pixbuf_err; // both errors are interesting; the fallback's is logged
                }
            },
        }
    }

    fn show_texture(&self, texture: gtk4::gdk::Texture) {
        self.picture.set_paintable(Some(&texture));
        self.picture.set_visible(true);
        self.placeholder.set_visible(false);
    }

    fn show_placeholder(&self) {
        self.picture.set_visible(false);
        self.placeholder.set_visible(true);
    }
}

/// Decodes an image file in-process (format sniffed from the content, so the cached file's
/// extension can't lie) and wraps the pixels in a `gdk::MemoryTexture`.
fn decode_texture_in_process(path: &Path) -> Result<gtk4::gdk::Texture, image::ImageError> {
    let bytes = std::fs::read(path)?;
    let decoded = image::load_from_memory(&bytes)?.into_rgba8();
    let (width, height) = decoded.dimensions();
    Ok(gtk4::gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        gtk4::gdk::MemoryFormat::R8g8b8a8,
        &gtk4::glib::Bytes::from_owned(decoded.into_raw()),
        (width * 4) as usize,
    )
    .into())
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

        // WebP has no gdk-pixbuf loader on the target distros — the in-process fallback must
        // pick it up (regression guard for the audiobookshelf-client cover pipeline).
        let webp = tmp.path().join("real.webp");
        write_1x1_webp(&webp);
        cover.set_path(Some(&webp));
        assert!(cover.picture.is_visible(), "a WebP cover should decode via the image-crate fallback");
        assert!(!cover.placeholder.is_visible());

        cover.set_path(None);
        assert!(cover.placeholder.is_visible(), "clearing the path restores the placeholder");
    }

    /// The smallest possible valid WebP (1x1, lossy VP8) — generated once with `cwebp`, so no
    /// WebP encoder is needed at test time.
    fn write_1x1_webp(path: &std::path::Path) {
        const WEBP_1X1: &[u8] = &[
            0x52, 0x49, 0x46, 0x46, 0x3c, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x20, 0x30, 0x00, 0x00, 0x00, 0xd0,
            0x01, 0x00, 0x9d, 0x01, 0x2a, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x34, 0x25, 0xa0, 0x02, 0x74, 0xba, 0x01, 0xf8, 0x00, 0x03,
            0xb0, 0x00, 0xfe, 0xf0, 0xc4, 0x0b, 0xff, 0x20, 0xb9, 0x61, 0x75, 0xc8, 0xd7, 0xff, 0x20, 0x3f, 0xe4, 0x07, 0xfc, 0x80, 0xff,
            0xf8, 0xf2, 0x00, 0x00, 0x00,
        ];
        std::fs::write(path, WEBP_1X1).unwrap();
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
