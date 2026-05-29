//! Phase Y3 — inline image rendering. The marker producer lives in
//! `crate::tools::read::format_image_marker`; the parser + rendering
//! decision live here.
//!
//! The module is split in two layers:
//!
//! - **`parse_image_marker`** is always compiled. It pulls
//!   `[Image: <abs-path> WxH FORMAT]` out of a tool-result string. The
//!   parser is needed even without `--features images` so the
//!   renderer can show a polished `[Image: foo.png 1024x768 PNG]`
//!   chip in the tool card instead of the raw line.
//! - **`render_image`** is gated `#[cfg(feature = "images")]`. With
//!   the feature on we build a `ratatui_image::Picker` from our own
//!   `Capabilities` (no DA1 probe — that hangs in buffer-terminals)
//!   and render via the stateless `Image` widget. Without the
//!   feature, callers fall back to plain text.

use crate::tui::caps::GraphicsKind;

/// Parsed `[Image: <abs-path> <WxH | ?x?> <FORMAT>]` marker. Lossless
/// round-trip with `crate::tools::read::format_image_marker`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageMarker {
    pub path: String,
    /// `None` when the producer couldn't determine dimensions.
    pub dims: Option<(u32, u32)>,
    /// Format name (e.g. `"PNG"`, `"JPEG"`).
    pub format: String,
}

impl ImageMarker {
    /// Short label suitable for a one-line summary. Used by the text
    /// fallback when image rendering is off or unavailable.
    #[allow(dead_code)] // Public API; only the test currently calls it directly.
    pub fn summary(&self) -> String {
        let name = std::path::Path::new(&self.path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&self.path);
        match self.dims {
            Some((w, h)) => format!("{} ({}x{} {})", name, w, h, self.format),
            None => format!("{} ({})", name, self.format),
        }
    }
}

/// Parse an `[Image: ...]` marker out of `s`. Returns the marker if
/// `s` is a single-line marker (optionally with trailing whitespace
/// or newlines). Returns `None` for anything else — including
/// multi-line tool results where the marker is embedded inside other
/// text (those flow through the regular text path).
pub fn parse_image_marker(s: &str) -> Option<ImageMarker> {
    let trimmed = s.trim();
    let body = trimmed.strip_prefix("[Image: ")?.strip_suffix(']')?;
    // body: "<abs-path> <WxH | ?x?> <FORMAT>"
    // Format is the last token, dims is the second-to-last, path is everything before.
    let last_space = body.rfind(' ')?;
    let (head, format) = body.split_at(last_space);
    let format = format.trim_start().to_string();
    let prev_space = head.rfind(' ')?;
    let (path, dims_str) = head.split_at(prev_space);
    let dims_str = dims_str.trim_start();
    let path = path.to_string();
    if path.is_empty() || format.is_empty() {
        return None;
    }
    let dims = parse_dims(dims_str);
    Some(ImageMarker { path, dims, format })
}

fn parse_dims(s: &str) -> Option<(u32, u32)> {
    if s == "?x?" {
        return None;
    }
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Whether `caps.graphics` + the build configuration support a true
/// image render (vs. the text fallback). Inside a buffer-terminal we
/// never attempt Kitty / Sixel — those escapes don't survive the host
/// — so the only positive answer there is `HalfBlock`.
pub fn can_render_images(graphics: GraphicsKind) -> bool {
    if !cfg!(feature = "images") {
        return false;
    }
    !matches!(graphics, GraphicsKind::None)
}

#[cfg(feature = "images")]
pub mod render {
    //! Real image rendering. Only compiled with `--features images`;
    //! the rest of the codebase calls in via thin wrappers in the
    //! parent module so non-image builds don't carry the
    //! `ratatui_image` symbols.

    use super::*;
    use ratatui::layout::Size;
    use ratatui_image::Resize;
    use ratatui_image::picker::{Picker, ProtocolType};
    use ratatui_image::protocol::Protocol;

    /// Build a `Picker` from our pre-detected `Capabilities` without
    /// issuing any terminal queries. We deliberately don't call
    /// `Picker::from_query_stdio()` because it hangs in
    /// buffer-terminals (Neovim's `:terminal`, VSCode's integrated
    /// terminal) — the same hosts where `caps.query_ok = false`.
    ///
    /// Font size defaults to a sensible terminal cell (8x16) when
    /// unknown; only affects pixel-to-cell math for half-block.
    pub fn picker_for_graphics(graphics: GraphicsKind) -> Picker {
        #[allow(deprecated)] // `from_query_stdio` would block; we already classified.
        let mut picker = Picker::from_fontsize((8, 16).into());
        let protocol = match graphics {
            GraphicsKind::Kitty => ProtocolType::Kitty,
            GraphicsKind::ITerm2 => ProtocolType::Iterm2,
            GraphicsKind::Sixel => ProtocolType::Sixel,
            GraphicsKind::HalfBlock | GraphicsKind::None => ProtocolType::Halfblocks,
        };
        picker.set_protocol_type(protocol);
        picker
    }

    /// Decode the image and build a stateless `Protocol` sized to the
    /// given `area`. Returns `Err(_)` if decoding or protocol
    /// construction fails — callers fall back to the text marker.
    #[allow(dead_code)] // Frame-level wiring lands in a follow-up.
    pub fn protocol_for(
        marker: &ImageMarker,
        graphics: GraphicsKind,
        area: Size,
    ) -> Result<Protocol, String> {
        let dyn_img = image::ImageReader::open(&marker.path)
            .map_err(|e| format!("open {}: {}", marker.path, e))?
            .with_guessed_format()
            .map_err(|e| format!("guess format {}: {}", marker.path, e))?
            .decode()
            .map_err(|e| format!("decode {}: {}", marker.path, e))?;
        let picker = picker_for_graphics(graphics);
        picker
            .new_protocol(dyn_img, area, Resize::Fit(None))
            .map_err(|e| format!("protocol {}: {}", marker.path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_marker_with_dimensions() {
        let m = parse_image_marker("[Image: /tmp/foo.png 1024x768 PNG]").unwrap();
        assert_eq!(m.path, "/tmp/foo.png");
        assert_eq!(m.dims, Some((1024, 768)));
        assert_eq!(m.format, "PNG");
    }

    #[test]
    fn parse_marker_with_unknown_dims() {
        let m = parse_image_marker("[Image: /tmp/foo.jpg ?x? JPEG]").unwrap();
        assert_eq!(m.dims, None);
        assert_eq!(m.format, "JPEG");
    }

    #[test]
    fn parse_marker_strips_trailing_whitespace() {
        let m = parse_image_marker("[Image: /tmp/foo.png 1x1 PNG]\n").unwrap();
        assert_eq!(m.format, "PNG");
    }

    #[test]
    fn parse_marker_rejects_non_marker_text() {
        assert!(parse_image_marker("hello world").is_none());
        assert!(parse_image_marker("[Image: only-one-token]").is_none());
        assert!(parse_image_marker("text before [Image: /a 1x1 PNG]").is_none());
        assert!(parse_image_marker("[Image:  ?x? PNG]").is_none());
    }

    #[test]
    fn parse_marker_round_trips_with_format_image_marker() {
        // The producer lives in tools::read; assert lossless round-trip
        // for both known-dim and unknown-dim paths.
        let s1 = crate::tools::read::format_image_marker("/abs/p.png", "png", Some((640, 480)));
        let m1 = parse_image_marker(&s1).unwrap();
        assert_eq!(m1.path, "/abs/p.png");
        assert_eq!(m1.dims, Some((640, 480)));
        assert_eq!(m1.format, "PNG");

        let s2 = crate::tools::read::format_image_marker("/abs/q.jpg", "jpeg", None);
        let m2 = parse_image_marker(&s2).unwrap();
        assert_eq!(m2.path, "/abs/q.jpg");
        assert_eq!(m2.dims, None);
        assert_eq!(m2.format, "JPEG");
    }

    #[test]
    fn summary_uses_basename_and_dimensions() {
        let m = ImageMarker {
            path: "/abs/path/cat.png".into(),
            dims: Some((1024, 768)),
            format: "PNG".into(),
        };
        assert_eq!(m.summary(), "cat.png (1024x768 PNG)");
    }

    #[test]
    fn summary_falls_back_when_dims_unknown() {
        let m = ImageMarker {
            path: "x.jpg".into(),
            dims: None,
            format: "JPEG".into(),
        };
        assert_eq!(m.summary(), "x.jpg (JPEG)");
    }

    #[test]
    fn can_render_images_off_without_feature_flag() {
        if !cfg!(feature = "images") {
            assert!(!can_render_images(GraphicsKind::Kitty));
            assert!(!can_render_images(GraphicsKind::HalfBlock));
        }
    }

    #[test]
    fn can_render_images_off_for_graphics_none() {
        // GraphicsKind::None is never renderable regardless of feature.
        assert!(!can_render_images(GraphicsKind::None));
    }

    #[cfg(feature = "images")]
    #[test]
    fn can_render_images_on_with_feature_and_capable_graphics() {
        assert!(can_render_images(GraphicsKind::Kitty));
        assert!(can_render_images(GraphicsKind::ITerm2));
        assert!(can_render_images(GraphicsKind::HalfBlock));
    }
}
