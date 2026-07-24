//! Semantic color palette for the TUI. Every renderer pulls
//! colors from a `Theme` instead of hardcoding `Color::Cyan`
//! etc., so swapping themes (`[ui].theme = "light"`) gives a
//! coherent recolor instead of a leopard-spotted mess.
//!
//! Three presets ship in-tree: `dark` (the historical default),
//! `light` (for white-background terminals), and `dimmed`
//! (low-contrast for OLED / late-night). `auto` consults
//! `$COLORFGBG` and picks `light` when the background is
//! visually light, otherwise `dark`.

use ratatui::style::Color;

/// Semantic color fields. Render functions take `&Theme` and
/// reference the role they want (`theme.accent`,
/// `theme.tool_err`, …), never the literal `Color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub user: Color,
    pub assistant: Color,
    pub tool_running: Color,
    pub tool_ok: Color,
    pub tool_err: Color,
    pub diff_added: Color,
    pub diff_removed: Color,
    pub match_highlight: Color,
    pub gauge_ok: Color,
    pub gauge_warn: Color,
    pub gauge_danger: Color,
    pub border: Color,
    /// Background tint for the user-message band and the composer.
    pub user_band_bg: Color,
    pub selected_fg: Color,
    pub selected_bg: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg: Color::Reset,
            fg: Color::White,
            dim: Color::DarkGray,
            accent: Color::Cyan,
            user: Color::Cyan,
            assistant: Color::White,
            tool_running: Color::Yellow,
            tool_ok: Color::Green,
            tool_err: Color::Red,
            diff_added: Color::Green,
            diff_removed: Color::Red,
            match_highlight: Color::Yellow,
            gauge_ok: Color::Green,
            gauge_warn: Color::Yellow,
            gauge_danger: Color::Red,
            border: Color::Cyan,
            user_band_bg: Color::Rgb(0x1f, 0x1f, 0x1f),
            selected_fg: Color::Black,
            selected_bg: Color::Cyan,
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color::Reset,
            fg: Color::Black,
            dim: Color::Gray,
            accent: Color::Blue,
            user: Color::Blue,
            assistant: Color::Black,
            tool_running: Color::Rgb(0xb5, 0x86, 0x00), // amber, readable on white
            tool_ok: Color::Rgb(0x00, 0x80, 0x00),
            tool_err: Color::Rgb(0xb0, 0x00, 0x00),
            diff_added: Color::Rgb(0x00, 0x80, 0x00),
            diff_removed: Color::Rgb(0xb0, 0x00, 0x00),
            match_highlight: Color::Rgb(0xb5, 0x86, 0x00),
            gauge_ok: Color::Rgb(0x00, 0x80, 0x00),
            gauge_warn: Color::Rgb(0xb5, 0x86, 0x00),
            gauge_danger: Color::Rgb(0xb0, 0x00, 0x00),
            border: Color::Blue,
            user_band_bg: Color::Rgb(0xf5, 0xf5, 0xf5),
            selected_fg: Color::White,
            selected_bg: Color::Blue,
        }
    }

    pub fn dimmed() -> Self {
        // Lower contrast everywhere; good for OLED / late-night.
        Self {
            bg: Color::Reset,
            fg: Color::Gray,
            dim: Color::DarkGray,
            accent: Color::Rgb(0x6c, 0x9a, 0xa0), // muted cyan
            user: Color::Rgb(0x6c, 0x9a, 0xa0),
            assistant: Color::Gray,
            tool_running: Color::Rgb(0x9a, 0x86, 0x4c),
            tool_ok: Color::Rgb(0x6c, 0x90, 0x6c),
            tool_err: Color::Rgb(0x9a, 0x6c, 0x6c),
            diff_added: Color::Rgb(0x6c, 0x90, 0x6c),
            diff_removed: Color::Rgb(0x9a, 0x6c, 0x6c),
            match_highlight: Color::Rgb(0x9a, 0x86, 0x4c),
            gauge_ok: Color::Rgb(0x6c, 0x90, 0x6c),
            gauge_warn: Color::Rgb(0x9a, 0x86, 0x4c),
            gauge_danger: Color::Rgb(0x9a, 0x6c, 0x6c),
            border: Color::Rgb(0x6c, 0x9a, 0xa0),
            user_band_bg: Color::Rgb(0x10, 0x10, 0x10),
            selected_fg: Color::Black,
            selected_bg: Color::Rgb(0x6c, 0x9a, 0xa0),
        }
    }
}

/// Resolve a theme by name. Unknown names fall back to `dark`.
/// `"auto"` reads `$COLORFGBG` and picks `light` when the
/// background half looks light, else `dark`.
pub fn load(name: &str) -> Theme {
    resolve(name, std::env::var("COLORFGBG").ok().as_deref())
}

/// Pure resolver, exposed for tests. `colorfgbg` mirrors the
/// `COLORFGBG` env var: `"15;0"` → fg=15, bg=0.
pub fn resolve(name: &str, colorfgbg: Option<&str>) -> Theme {
    match name.trim().to_ascii_lowercase().as_str() {
        "light" => Theme::light(),
        "dimmed" | "dim" => Theme::dimmed(),
        "auto" => {
            if colorfgbg_indicates_light(colorfgbg) {
                Theme::light()
            } else {
                Theme::dark()
            }
        }
        _ => Theme::dark(),
    }
}

/// `COLORFGBG` is `"<fg>;<bg>"` or `"<fg>;<extra>;<bg>"` where
/// each number is a 16-color ANSI index. Background indices in
/// `{7, 15}` (white-ish) and `{11}` (yellow) read as light.
fn colorfgbg_indicates_light(s: Option<&str>) -> bool {
    let Some(s) = s else {
        return false;
    };
    let bg = s.rsplit(';').next().unwrap_or("");
    let n: u8 = match bg.trim().parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    matches!(n, 7 | 11 | 15)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_name_falls_back_to_dark() {
        assert_eq!(resolve("nonsense", None), Theme::dark());
    }

    #[test]
    fn light_name_returns_light_theme() {
        assert_eq!(resolve("light", None), Theme::light());
    }

    #[test]
    fn dimmed_and_dim_both_work() {
        assert_eq!(resolve("dimmed", None), Theme::dimmed());
        assert_eq!(resolve("dim", None), Theme::dimmed());
    }

    #[test]
    fn name_is_case_insensitive_and_trimmed() {
        assert_eq!(resolve("  LIGHT  ", None), Theme::light());
    }

    #[test]
    fn auto_returns_light_for_light_bg() {
        // COLORFGBG="0;15" — fg=black, bg=white.
        assert_eq!(resolve("auto", Some("0;15")), Theme::light());
    }

    #[test]
    fn auto_returns_dark_for_dark_bg() {
        // COLORFGBG="15;0" — fg=white, bg=black.
        assert_eq!(resolve("auto", Some("15;0")), Theme::dark());
    }

    #[test]
    fn auto_handles_three_field_form() {
        // COLORFGBG="0;default;15" — middle is sometimes "default".
        assert_eq!(resolve("auto", Some("0;default;15")), Theme::light());
    }

    #[test]
    fn auto_with_no_env_var_defaults_to_dark() {
        assert_eq!(resolve("auto", None), Theme::dark());
    }

    #[test]
    fn auto_with_malformed_env_var_defaults_to_dark() {
        assert_eq!(resolve("auto", Some("garbage")), Theme::dark());
    }
}
