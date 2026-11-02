//! TDM Dark and TDM Light.
//!
//! Every colour a view draws comes from a [`Palette`]. Style functions take
//! the iced `Theme` and recover the palette from it with [`Palette::of`], so a
//! widget styled once follows the theme switch — including the cross-fade,
//! which is just a [`Palette::mix`] of the two.

use iced::border::Radius;
use iced::gradient::Linear;
use iced::widget::{button, container, progress_bar, text, text_input};
use iced::{Background, Border, Color, Gradient, Radians, Shadow, Theme, Vector};

pub const fn rgb(value: u32) -> Color {
    Color {
        r: ((value >> 16) & 0xFF) as f32 / 255.0,
        g: ((value >> 8) & 0xFF) as f32 / 255.0,
        b: (value & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Window chrome behind everything.
    pub bg: Color,
    /// Sidebar, header bands, status bar.
    pub surface: Color,
    /// Cards: list rows, dialogs.
    pub card: Color,
    /// Hover and selection fill, menus.
    pub raised: Color,
    pub border: Color,
    pub text: Color,
    /// Secondary text. Still held to body-text contrast.
    pub muted: Color,
    /// Gradient start and end of the flame accent.
    pub accent: Color,
    pub accent2: Color,
    /// Text drawn on the accent.
    pub on_accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub shadow: Color,
}

impl Palette {
    pub const fn dark() -> Self {
        Self {
            bg: rgb(0x121010),
            surface: rgb(0x1A1715),
            card: rgb(0x221D1A),
            raised: rgb(0x2E2622),
            border: rgb(0x3B312B),
            text: rgb(0xF6EEE7),
            muted: rgb(0xB8AA9E),
            accent: rgb(0xFF7A1A),
            accent2: rgb(0xFFB347),
            // White text on the orange buttons in dark mode.
            on_accent: rgb(0xFFFFFF),
            success: rgb(0x52C98F),
            warning: rgb(0xFFC857),
            danger: rgb(0xFF6B5E),
            shadow: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 },
        }
    }

    pub const fn light() -> Self {
        Self {
            bg: rgb(0xF7F2EE),
            surface: rgb(0xF0E8E1),
            card: rgb(0xFFFFFF),
            raised: rgb(0xF4E6DA),
            border: rgb(0xE2D4C7),
            text: rgb(0x2A1F18),
            muted: rgb(0x6B5B4F),
            accent: rgb(0xF06A10),
            accent2: rgb(0xFFA83A),
            on_accent: rgb(0x1A0F08),
            success: rgb(0x1E7F53),
            warning: rgb(0x9A6200),
            danger: rgb(0xC9362B),
            shadow: Color { r: 0.35, g: 0.2, b: 0.1, a: 0.16 },
        }
    }

    /// `t` = 0 is dark, 1 is light; anything between is the cross-fade.
    pub fn at(t: f32) -> Self {
        Self::dark().mix(&Self::light(), t)
    }

    pub fn mix(&self, other: &Self, t: f32) -> Self {
        let m = |a, b| blend(a, b, t);
        Self {
            bg: m(self.bg, other.bg),
            surface: m(self.surface, other.surface),
            card: m(self.card, other.card),
            raised: m(self.raised, other.raised),
            border: m(self.border, other.border),
            text: m(self.text, other.text),
            muted: m(self.muted, other.muted),
            accent: m(self.accent, other.accent),
            accent2: m(self.accent2, other.accent2),
            on_accent: m(self.on_accent, other.on_accent),
            success: m(self.success, other.success),
            warning: m(self.warning, other.warning),
            danger: m(self.danger, other.danger),
            shadow: m(self.shadow, other.shadow),
        }
    }

    /// The palette an iced theme was built from. The background's red channel
    /// is linear in the mix factor, so this recovers mid-fade palettes exactly.
    pub fn of(theme: &Theme) -> Self {
        let (d, l) = (Self::dark().bg.r, Self::light().bg.r);
        Self::at((theme.palette().background.r - d) / (l - d))
    }

    pub fn theme(&self) -> Theme {
        Theme::custom(
            "TDM",
            iced::theme::Palette {
                background: self.bg,
                text: self.text,
                primary: self.accent,
                success: self.success,
                warning: self.warning,
                danger: self.danger,
            },
        )
    }

    /// The flame gradient, left to right.
    pub fn flame(&self) -> Background {
        Background::Gradient(Gradient::Linear(
            Linear::new(Radians(std::f32::consts::FRAC_PI_2))
                .add_stop(0.0, self.accent)
                .add_stop(1.0, self.accent2),
        ))
    }
}

pub fn alpha(c: Color, a: f32) -> Color {
    Color { a: c.a * a, ..c }
}

// ------------------------------------------------------------------ styles

pub fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(Palette::of(theme).muted) }
}

pub fn chrome(theme: &Theme) -> container::Style {
    let p = Palette::of(theme);
    container::Style {
        background: Some(p.bg.into()),
        text_color: Some(p.text),
        ..Default::default()
    }
}

pub fn surface(theme: &Theme) -> container::Style {
    let p = Palette::of(theme);
    container::Style {
        background: Some(p.surface.into()),
        text_color: Some(p.text),
        ..Default::default()
    }
}

pub fn card(theme: &Theme) -> container::Style {
    let p = Palette::of(theme);
    container::Style {
        background: Some(p.card.into()),
        text_color: Some(p.text),
        border: Border { color: p.border, width: 1.0, radius: 12.0.into() },
        shadow: Shadow { color: p.shadow, offset: Vector::new(0.0, 6.0), blur_radius: 24.0 },
        ..Default::default()
    }
}

pub fn separator(theme: &Theme) -> container::Style {
    container::Style { background: Some(Palette::of(theme).border.into()), ..Default::default() }
}

/// A tinted, fully rounded status badge.
pub fn pill(colour: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(alpha(colour, 0.16).into()),
        text_color: Some(colour),
        border: Border { radius: Radius::from(99.0), ..Default::default() },
        ..Default::default()
    }
}

/// Transparent until hovered; `active` keeps the hover fill (open menu,
/// selected tab).
pub fn flat(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let p = Palette::of(theme);
        let fill = match status {
            _ if active => Some(p.raised.into()),
            button::Status::Hovered => Some(p.raised.into()),
            button::Status::Pressed => Some(p.border.into()),
            _ => None,
        };
        button::Style {
            background: fill,
            text_color: if status == button::Status::Disabled { alpha(p.muted, 0.6) } else { p.text },
            border: Border { radius: 8.0.into(), ..Default::default() },
            ..Default::default()
        }
    }
}

/// A list row: like [`flat`], with an accent edge when selected. `marked`
/// (0..=1) is how far the row is ticked in select mode; a marked row takes
/// an accent wash and edge in proportion.
pub fn row_button(selected: bool, marked: f32) -> impl Fn(&Theme, button::Status) -> button::Style {
    let marked = marked.clamp(0.0, 1.0);
    move |theme, status| {
        let p = Palette::of(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let base = if selected {
            p.raised
        } else if hovered {
            alpha(p.raised, 0.6)
        } else {
            p.card
        };
        let edged = selected || marked > 0.0;
        button::Style {
            background: Some(blend(base, p.accent, 0.12 * marked).into()),
            text_color: p.text,
            border: Border {
                color: if edged { p.accent } else { p.border },
                width: if edged { 1.5 } else { 1.0 },
                radius: 10.0.into(),
            },
            ..Default::default()
        }
    }
}

pub fn side_button(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let p = Palette::of(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: (selected || hovered).then(|| alpha(p.raised, if selected { 1.0 } else { 0.6 }).into()),
            text_color: p.text,
            border: Border { radius: 8.0.into(), ..Default::default() },
            ..Default::default()
        }
    }
}

/// WCAG relative luminance of an sRGB colour.
fn luminance(c: Color) -> f32 {
    let lin = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio between two colours.
fn contrast(a: Color, b: Color) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// The fill of a primary button: the accent itself when the button text reads
/// well on it, otherwise a slightly darker shade, so the text stays at least
/// 4.5:1. The brand accent is unchanged everywhere else.
pub fn button_fill(p: &Palette) -> Color {
    let mut fill = p.accent;
    for _ in 0..24 {
        if contrast(p.on_accent, fill) >= 4.5 {
            break;
        }
        fill = Color { r: fill.r * 0.94, g: fill.g * 0.94, b: fill.b * 0.94, ..fill };
    }
    fill
}

/// The primary button: one solid colour in every state (no gradient). Only
/// the shadow and the disabled fill change.
pub fn accent(theme: &Theme, status: button::Status) -> button::Style {
    let p = Palette::of(theme);
    let fill = button_fill(&p);
    let (bg, shadow) = match status {
        button::Status::Disabled => (alpha(p.muted, 0.3), Color::TRANSPARENT),
        _ => (fill, alpha(fill, 0.25)),
    };
    button::Style {
        background: Some(bg.into()),
        text_color: p.on_accent,
        border: Border { radius: 8.0.into(), ..Default::default() },
        shadow: Shadow { color: shadow, offset: Vector::new(0.0, 2.0), blur_radius: 10.0 },
        ..Default::default()
    }
}

pub fn danger(theme: &Theme, status: button::Status) -> button::Style {
    let p = Palette::of(theme);
    button::Style {
        background: Some(if status == button::Status::Hovered { p.danger } else { alpha(p.danger, 0.85) }.into()),
        text_color: p.on_accent,
        border: Border { radius: 8.0.into(), ..Default::default() },
        ..Default::default()
    }
}

/// Inputs sit on the card colour with a border that lights up on focus.
pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let p = Palette::of(theme);
    let edge = match status {
        text_input::Status::Focused { .. } => p.accent,
        text_input::Status::Hovered => p.muted,
        _ => p.border,
    };
    text_input::Style {
        background: p.card.into(),
        border: Border { color: edge, width: 1.0, radius: 8.0.into() },
        icon: p.muted,
        placeholder: p.muted,
        value: p.text,
        selection: alpha(p.accent, 0.35),
    }
}

/// Borderless input for use inside a styled container (the search field).
pub fn bare_input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Color::TRANSPARENT.into(),
        border: Border::default(),
        ..input(theme, status)
    }
}

/// The flame gradient bar. `shimmer` is a 0..1 phase for a running download:
/// a soft highlight sweeps across the filled part. `None` draws it still.
pub fn progress(colour: Option<Color>, shimmer: Option<f32>) -> impl Fn(&Theme) -> progress_bar::Style {
    move |theme| {
        let p = Palette::of(theme);
        let bar = match (colour, shimmer) {
            (Some(c), _) => c.into(),
            (None, None) => p.flame(),
            (None, Some(phase)) => {
                let mut g = Linear::new(Radians(std::f32::consts::FRAC_PI_2)).add_stop(0.0, p.accent);
                // A highlight band 0.3 wide whose centre runs from -0.15 to
                // 1.15, so it enters and leaves the bar smoothly.
                let c = phase * 1.3 - 0.15;
                let glow = blend(p.accent2, Color::WHITE, 0.35);
                let base = |o: f32| blend(p.accent, p.accent2, o);
                for (o, col) in [(c - 0.15, base(c - 0.15)), (c, glow), (c + 0.15, base(c + 0.15))] {
                    if o > 0.0 && o < 1.0 {
                        g = g.add_stop(o, col);
                    }
                }
                Background::Gradient(Gradient::Linear(g.add_stop(1.0, p.accent2)))
            }
        };
        progress_bar::Style {
            background: alpha(p.border, 0.7).into(),
            bar,
            border: Border { radius: 99.0.into(), ..Default::default() },
        }
    }
}

/// Linear blend from `a` to `b`, clamped.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    // Weighted sum rather than `a + (b - a) * t`, so both ends are exact.
    let t = t.clamp(0.0, 1.0);
    let m = |x: f32, y: f32| x * (1.0 - t) + y * t;
    Color { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b), a: m(a.a, b.a) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lum(c: Color) -> f32 {
        let f = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
    }

    fn ratio(a: Color, b: Color) -> f32 {
        let (x, y) = (lum(a), lum(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn text_stays_readable_in_both_palettes() {
        for (name, p) in [("dark", Palette::dark()), ("light", Palette::light())] {
            for (bg_name, bg) in [("bg", p.bg), ("surface", p.surface), ("card", p.card), ("raised", p.raised)] {
                for (fg_name, fg) in [("text", p.text), ("muted", p.muted)] {
                    let r = ratio(fg, bg);
                    assert!(r >= 4.5, "{name}: {fg_name} on {bg_name} is only {r:.2}:1");
                }
            }
            for (c_name, c) in [("success", p.success), ("warning", p.warning), ("danger", p.danger)] {
                let r = ratio(c, p.card);
                assert!(r >= 4.5, "{name}: {c_name} text on card is only {r:.2}:1");
            }
            let fill = button_fill(&p);
            let r = ratio(p.on_accent, fill);
            assert!(r >= 4.5, "{name}: button text on the button fill is only {r:.2}:1");
        }
    }

    #[test]
    fn the_palette_survives_a_trip_through_an_iced_theme() {
        for t in [0.0, 0.25, 0.5, 1.0] {
            let p = Palette::at(t);
            let back = Palette::of(&p.theme());
            assert!((back.text.r - p.text.r).abs() < 1e-4, "t={t}");
            assert!((back.accent.g - p.accent.g).abs() < 1e-4, "t={t}");
        }
    }

    #[test]
    fn mix_hits_both_ends() {
        assert_eq!(Palette::at(0.0), Palette::dark());
        assert_eq!(Palette::at(1.0), Palette::light());
        assert_eq!(Palette::at(-3.0), Palette::dark(), "clamped");
    }
}
