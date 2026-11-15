//! Colors and shared styles.
//!
//! The palette follows the defaults of Charm's terminal libraries (lipgloss,
//! bubbles, huh): violet title pills, fuchsia selection, hot pink for what
//! needs you, and quiet grays for everything secondary. Only the color values
//! were taken; no code was copied.
//!
//! Colors are written as 24-bit values and lowered to the 256-color or
//! 16-color palette when the terminal does not take 24-bit color, so the
//! look holds up in Terminal.app or the Linux console. `NO_COLOR` turns all
//! color off; every state stays readable from its glyph alone.

use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

/// How many colors the terminal shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorLevel {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

impl ColorLevel {
    /// From the environment (`NO_COLOR`, `COLORTERM`, `TERM`).
    pub fn detect(env: impl Fn(&str) -> Option<String>) -> ColorLevel {
        if env("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return ColorLevel::None;
        }
        let term = env("TERM").unwrap_or_default().to_ascii_lowercase();
        let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm == "truecolor" || colorterm == "24bit" {
            // GNU screen passes COLORTERM through but only shows 256 colors.
            let tmux = env("TMUX").is_some() || env("TERM_PROGRAM").as_deref() == Some("tmux");
            if term.starts_with("screen") && !tmux {
                return ColorLevel::Ansi256;
            }
            return ColorLevel::TrueColor;
        }
        match term.as_str() {
            "" | "dumb" => return ColorLevel::None,
            "alacritty" | "foot" | "foot-extra" | "wezterm" | "xterm-kitty" | "xterm-ghostty"
            | "contour" | "rio" => return ColorLevel::TrueColor,
            _ => {}
        }
        if term.contains("256color") {
            ColorLevel::Ansi256
        } else {
            ColorLevel::Ansi16
        }
    }
}

/// Whether the terminal background is dark. `COLORFGBG` ("15;0") is the only
/// cheap hint; without it, dark (most terminals).
pub fn detect_dark(env: impl Fn(&str) -> Option<String>) -> bool {
    let Some(v) = env("COLORFGBG") else {
        return true;
    };
    let bg = v
        .rsplit(';')
        .next()
        .and_then(|bg| bg.trim().parse::<u8>().ok());
    !matches!(bg, Some(7 | 15))
}

/// A 24-bit color, written as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u32);

impl Rgb {
    fn parts(self) -> (u8, u8, u8) {
        ((self.0 >> 16) as u8, (self.0 >> 8) as u8, self.0 as u8)
    }

    /// The color `t` (0..=1) of the way from `self` to `to`.
    pub fn mix(self, to: Rgb, t: f64) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let (a, b) = (self.parts(), to.parts());
        let ch = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u32;
        Rgb(ch(a.0, b.0) << 16 | ch(a.1, b.1) << 8 | ch(a.2, b.2))
    }
}

/// One palette entry: `dark` on a dark background, `light` on a light one
/// (lipgloss's AdaptiveColor), and the ANSI color used when the terminal has
/// only 16 colors. Nearest-color matching is not good enough there: the
/// faint grays would turn black on a black background.
#[derive(Clone, Copy, Debug)]
struct Slot {
    dark: u32,
    light: u32,
    ansi: Color,
}

const fn slot(dark: u32, light: u32, ansi: Color) -> Slot {
    Slot { dark, light, ansi }
}

/// The palette. Values from bubbles (list, help, progress), lipgloss's
/// examples and huh's Charm theme.
mod hex {
    use super::{Slot, slot};
    use ratatui::style::Color::*;
    /// Descriptions and secondary text (bubbles list NormalDesc).
    pub const MUTED: Slot = slot(0x777777, 0xA49FA5, DarkGray);
    /// Hints and help keys (bubbles help key).
    pub const SUBTLE: Slot = slot(0x626262, 0x909090, DarkGray);
    /// Separators, inactive borders (bubbles verySubduedColor).
    pub const FAINT: Slot = slot(0x3C3C3C, 0xDDDADA, DarkGray);
    /// Title pills and focus (lipgloss highlight).
    pub const BRAND: Slot = slot(0x7D56F4, 0x874BFD, Magenta);
    /// Text on a colored pill.
    pub const CREAM: Slot = slot(0xFFFDF5, 0xFFFDF5, White);
    /// Dark text on a light pill.
    pub const INK: Slot = slot(0x1A1A1A, 0x1A1A1A, Black);
    /// The selected item (bubbles list SelectedTitle).
    pub const ACCENT: Slot = slot(0xEE6FF8, 0xEE6FF8, LightMagenta);
    /// The selected item's bar and description (bubbles list SelectedDesc).
    pub const ACCENT_DIM: Slot = slot(0xAD58B4, 0xF793FF, Magenta);
    /// Program names (huh's indigo).
    pub const INDIGO: Slot = slot(0x7571F9, 0x5A56E0, LightBlue);
    pub const PINK: Slot = slot(0xFF5F87, 0xFF4672, LightRed);
    pub const GREEN: Slot = slot(0x04B575, 0x02BA84, Green);
    pub const YELLOW: Slot = slot(0xECFD65, 0xB5A400, Yellow);
    pub const GRAY: Slot = slot(0xA8A8A8, 0x808080, Gray);
    /// Status bar (lipgloss's status bar example).
    pub const BAR_BG: Slot = slot(0x353533, 0xD9DCCF, DarkGray);
    pub const BAR_FG: Slot = slot(0xC1C6B2, 0x343433, White);
}

/// Ends of the progress-bar gradient (bubbles progress default).
pub const GRADIENT: (Rgb, Rgb) = (Rgb(0x5A56E0), Rgb(0xEE6FF8));

/// Resolved colors for this terminal.
#[derive(Clone, Debug)]
pub struct Theme {
    pub level: ColorLevel,
    pub muted: Color,
    pub subtle: Color,
    pub faint: Color,
    pub brand: Color,
    pub cream: Color,
    pub ink: Color,
    pub accent: Color,
    pub accent_dim: Color,
    pub indigo: Color,
    pub pink: Color,
    pub green: Color,
    pub yellow: Color,
    pub gray: Color,
    pub bar_bg: Color,
    pub bar_fg: Color,
}

impl Theme {
    pub fn new(level: ColorLevel, dark: bool) -> Theme {
        let c = |s: Slot| match level {
            ColorLevel::Ansi16 => s.ansi,
            _ => color(level, Rgb(if dark { s.dark } else { s.light })),
        };
        Theme {
            level,
            muted: c(hex::MUTED),
            subtle: c(hex::SUBTLE),
            faint: c(hex::FAINT),
            brand: c(hex::BRAND),
            cream: c(hex::CREAM),
            ink: c(hex::INK),
            accent: c(hex::ACCENT),
            accent_dim: c(hex::ACCENT_DIM),
            indigo: c(hex::INDIGO),
            pink: c(hex::PINK),
            green: c(hex::GREEN),
            yellow: c(hex::YELLOW),
            gray: c(hex::GRAY),
            bar_bg: c(hex::BAR_BG),
            bar_fg: c(hex::BAR_FG),
        }
    }

    pub fn has_color(&self) -> bool {
        self.level != ColorLevel::None
    }

    /// A 24-bit color lowered for this terminal.
    pub fn rgb(&self, c: Rgb) -> Color {
        color(self.level, c)
    }

    pub fn fg(&self, c: Color) -> Style {
        Style::new().fg(c)
    }

    /// A label on a colored background (lipgloss's title pill). Without
    /// color, reversed so it still stands out.
    pub fn pill(&self, bg: Color, fg: Color) -> Style {
        if self.has_color() {
            Style::new().bg(bg).fg(fg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    /// The highlighted row of a table (bubbles table: cream on violet).
    pub fn selected_row(&self) -> Style {
        self.pill(self.brand, self.cream)
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Fix the theme before the first draw. `dark: None` detects it.
pub fn init(dark: Option<bool>) {
    let env = |k: &str| std::env::var(k).ok();
    let dark = dark.unwrap_or_else(|| detect_dark(env));
    let _ = THEME.set(Theme::new(ColorLevel::detect(env), dark));
}

/// The theme (from the environment when [`init`] was not called).
pub fn t() -> &'static Theme {
    THEME.get_or_init(|| {
        let env = |k: &str| std::env::var(k).ok();
        Theme::new(ColorLevel::detect(env), detect_dark(env))
    })
}

fn color(level: ColorLevel, c: Rgb) -> Color {
    let (r, g, b) = c.parts();
    match level {
        ColorLevel::TrueColor => Color::Rgb(r, g, b),
        ColorLevel::Ansi256 => Color::Indexed(to_ansi256(r, g, b)),
        ColorLevel::Ansi16 => to_ansi16(r, g, b),
        ColorLevel::None => Color::Reset,
    }
}

fn dist(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2) as u32;
    // Weighted for how the eye sees green over blue.
    2 * d(a.0, b.0) + 4 * d(a.1, b.1) + 3 * d(a.2, b.2)
}

/// The nearest entry of the xterm 256-color palette: the 6×6×6 cube or the
/// 24-step gray ramp.
fn to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let step = |v: u8| -> usize {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, l)| (v as i32 - **l as i32).abs())
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let (qr, qg, qb) = (step(r), step(g), step(b));
    let cube = (LEVELS[qr], LEVELS[qg], LEVELS[qb]);
    let cube_index = 16 + 36 * qr as u8 + 6 * qg as u8 + qb as u8;
    let avg = ((r as u32 + g as u32 + b as u32) / 3) as i32;
    let gray_step = ((avg - 8 + 5) / 10).clamp(0, 23) as u8;
    let gray = 8 + 10 * gray_step;
    if dist((r, g, b), (gray, gray, gray)) < dist((r, g, b), cube) {
        232 + gray_step
    } else {
        cube_index
    }
}

/// The nearest of the 16 ANSI colors (xterm's defaults). The terminal's own
/// theme then decides how they look.
fn to_ansi16(r: u8, g: u8, b: u8) -> Color {
    const TABLE: [(Color, (u8, u8, u8)); 16] = [
        (Color::Black, (0, 0, 0)),
        (Color::Red, (205, 0, 0)),
        (Color::Green, (0, 205, 0)),
        (Color::Yellow, (205, 205, 0)),
        (Color::Blue, (0, 0, 238)),
        (Color::Magenta, (205, 0, 205)),
        (Color::Cyan, (0, 205, 205)),
        (Color::Gray, (229, 229, 229)),
        (Color::DarkGray, (127, 127, 127)),
        (Color::LightRed, (255, 0, 0)),
        (Color::LightGreen, (0, 255, 0)),
        (Color::LightYellow, (255, 255, 0)),
        (Color::LightBlue, (92, 92, 255)),
        (Color::LightMagenta, (255, 0, 255)),
        (Color::LightCyan, (0, 255, 255)),
        (Color::White, (255, 255, 255)),
    ];
    TABLE
        .iter()
        .min_by_key(|(_, v)| dist((r, g, b), *v))
        .map(|(c, _)| *c)
        .unwrap_or(Color::Reset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn detects_color_level() {
        use ColorLevel::*;
        let cases: &[(&[(&str, &str)], ColorLevel)] = &[
            (
                &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")],
                TrueColor,
            ),
            (&[("TERM", "xterm-256color")], Ansi256),
            (&[("TERM", "xterm-kitty")], TrueColor),
            (
                &[("TERM", "screen-256color"), ("COLORTERM", "truecolor")],
                Ansi256,
            ),
            (
                &[
                    ("TERM", "screen-256color"),
                    ("COLORTERM", "truecolor"),
                    ("TMUX", "/tmp/t,1,0"),
                ],
                TrueColor,
            ),
            (&[("TERM", "linux")], Ansi16),
            (&[("TERM", "dumb")], None),
            (&[("TERM", "xterm-256color"), ("NO_COLOR", "1")], None),
            // An empty NO_COLOR does not count (no-color.org).
            (&[("TERM", "xterm-256color"), ("NO_COLOR", "")], Ansi256),
        ];
        for (vars, want) in cases {
            assert_eq!(ColorLevel::detect(env(vars)), *want, "{vars:?}");
        }
    }

    #[test]
    fn detects_light_background() {
        assert!(detect_dark(env(&[])));
        assert!(detect_dark(env(&[("COLORFGBG", "15;0")])));
        assert!(!detect_dark(env(&[("COLORFGBG", "0;15")])));
        assert!(!detect_dark(env(&[("COLORFGBG", "0;default;7")])));
        assert!(detect_dark(env(&[("COLORFGBG", "garbage")])));
    }

    #[test]
    fn lowers_to_256_colors() {
        // Exact cube and gray entries map to themselves.
        assert_eq!(to_ansi256(0x5f, 0x5f, 0xd7), 62);
        assert_eq!(to_ansi256(0xff, 0x87, 0xaf), 211);
        assert_eq!(to_ansi256(0x3a, 0x3a, 0x3a), 237);
        assert_eq!(to_ansi256(0, 0, 0), 16);
        assert_eq!(to_ansi256(0xff, 0xff, 0xff), 231);
        // Charm's violet lands on a violet, not a gray.
        let i = to_ansi256(0x7D, 0x56, 0xF4);
        assert!((16..232).contains(&i), "{i}");
    }

    #[test]
    fn lowers_to_16_colors() {
        assert_eq!(to_ansi16(250, 10, 10), Color::LightRed);
        assert_eq!(to_ansi16(10, 10, 10), Color::Black);
        assert_eq!(to_ansi16(120, 120, 120), Color::DarkGray);
        assert_eq!(to_ansi16(0x5A, 0x56, 0xE0), Color::LightBlue);
        // The palette's own entries use fixed ANSI colors instead, so the
        // faint grays stay visible on a black background.
        let th = Theme::new(ColorLevel::Ansi16, true);
        assert_eq!(th.faint, Color::DarkGray);
        assert_eq!(th.pink, Color::LightRed);
    }

    #[test]
    fn no_color_pills_are_reversed() {
        let th = Theme::new(ColorLevel::None, true);
        assert_eq!(th.brand, Color::Reset);
        assert!(
            th.pill(th.brand, th.cream)
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn mixes_colors() {
        let (a, b) = GRADIENT;
        assert_eq!(a.mix(b, 0.0), a);
        assert_eq!(a.mix(b, 1.0), b);
        assert_eq!(Rgb(0x000000).mix(Rgb(0xFFFFFF), 0.5), Rgb(0x808080));
    }
}
