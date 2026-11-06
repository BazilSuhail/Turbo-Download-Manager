//! TDM Dark and TDM Light.
//!
//! Every colour a view draws comes from a [`Palette`]. Style functions take
//! the iced `Theme` and recover the palette from it with [`Palette::of`], so a
//! widget styled once follows the theme switch — including the cross-fade,
//! which is just a [`Palette::mix`] of the two.

use iced::border::Radius;
use iced::gradient::Linear;
use iced::widget::overlay::menu;
use iced::widget::{
    button, checkbox as checkbox_w, container, pick_list, progress_bar, text, text_editor, text_input,
};
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
    /// Control edges (inputs, unchecked boxes): at least 3:1 against the card.
    pub border_strong: Color,
    pub text: Color,
    /// Secondary text. Still held to body-text contrast.
    pub muted: Color,
    /// Gradient start and end of the accent.
    pub accent: Color,
    pub accent2: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub shadow: Color,
}

impl Palette {
    pub const fn dark() -> Self {
        Self {
            bg: rgb(0x000000),
            surface: rgb(0x0A0A0B),
            card: rgb(0x111113),
            raised: rgb(0x1B1B1E),
            border: rgb(0x2A2A2E),
            border_strong: rgb(0x6B6B73),
            text: rgb(0xF2F2F3),
            muted: rgb(0x9B9BA3),
            accent: rgb(0xFF7A1A),
            accent2: rgb(0xFFB347),
            success: rgb(0x34C77B),
            warning: rgb(0xE5A93A),
            danger: rgb(0xF2665C),
            shadow: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.5 },
        }
    }

    pub const fn light() -> Self {
        Self {
            bg: rgb(0xFFFFFF),
            surface: rgb(0xF5F5F7),
            card: rgb(0xFFFFFF),
            raised: rgb(0xEEEEF2),
            border: rgb(0xDCDCE2),
            border_strong: rgb(0x8A8A94),
            text: rgb(0x111114),
            muted: rgb(0x5F5F68),
            accent: rgb(0xF06A10),
            accent2: rgb(0xFFA83A),
            success: rgb(0x167A4A),
            warning: rgb(0x8A5A00),
            danger: rgb(0xC02A20),
            shadow: Color { r: 0.2, g: 0.2, b: 0.2, a: 0.14 },
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
            border_strong: m(self.border_strong, other.border_strong),
            text: m(self.text, other.text),
            muted: m(self.muted, other.muted),
            accent: m(self.accent, other.accent),
            accent2: m(self.accent2, other.accent2),
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

    /// The accent gradient (accent to accent2), left to right. The method
    /// name is kept for existing callers.
    pub fn flame(&self) -> Background {
        Background::Gradient(Gradient::Linear(
            Linear::new(Radians(std::f32::consts::FRAC_PI_2))
                .add_stop(0.0, self.accent)
                .add_stop(1.0, self.accent2),
        ))
    }
}

/// True when the palette is the light one (or past the halfway point of the fade).
pub fn is_light(p: &Palette) -> bool {
    p.bg.r > 0.5
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
            text_color: if status == button::Status::Disabled { alpha(p.muted, 0.85) } else { p.text },
            border: Border { radius: 8.0.into(), ..Default::default() },
            ..Default::default()
        }
    }
}

/// [`flat`] with a 1px edge: for Cancel and other quiet dialog buttons.
pub fn ghost(theme: &Theme, status: button::Status) -> button::Style {
    let p = Palette::of(theme);
    button::Style {
        border: Border { color: p.border, width: 1.0, radius: 8.0.into() },
        ..flat(false)(theme, status)
    }
}

/// A toggle chip: filled and edged in the accent when `active`.
pub fn chip(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let p = Palette::of(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let background = if active {
            Some(blend(p.card, p.accent, if hovered { 0.26 } else { 0.18 }).into())
        } else if hovered {
            Some(p.raised.into())
        } else {
            None
        };
        button::Style {
            background,
            text_color: if status == button::Status::Disabled { alpha(p.muted, 0.85) } else { p.text },
            border: Border {
                color: if active { p.accent } else { p.border },
                width: 1.0,
                radius: 99.0.into(),
            },
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
        let pressed = status == button::Status::Pressed;
        let base = if selected {
            p.raised
        } else if hovered {
            alpha(p.raised, 0.6)
        } else {
            p.card
        };
        let edged = selected || marked > 0.0;
        button::Style {
            background: Some(
                blend(blend(base, p.accent, 0.12 * marked), p.accent, if pressed { 0.08 } else { 0.0 }).into(),
            ),
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
        let hovered = status == button::Status::Hovered;
        let pressed = status == button::Status::Pressed;
        let background = if pressed {
            Some(p.border.into())
        } else if selected {
            Some(blend(p.surface, p.accent, 0.14).into())
        } else if hovered {
            Some(alpha(p.raised, 0.6).into())
        } else {
            None
        };
        button::Style {
            background,
            text_color: p.text,
            border: Border {
                color: if selected { p.accent } else { Color::TRANSPARENT },
                width: if selected { 1.0 } else { 0.0 },
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    }
}

/// WCAG relative luminance of an sRGB colour.
fn luminance(c: Color) -> f32 {
    let lin = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// The fill of every accent button: a deeper orange than the foreground accent,
/// so the button reads as solid in both themes. Its text comes from `on_button`.
pub fn button_fill(_p: &Palette) -> Color {
    rgb(0xC2410C)
}

/// The text and icon colour on an accent button: black or white, whichever contrasts more.
pub fn on_button(p: &Palette) -> Color {
    readable_on(button_fill(p))
}

/// The primary button: one solid colour in every state (no gradient). Only
/// the shadow and the disabled fill change.
pub fn accent(theme: &Theme, status: button::Status) -> button::Style {
    let p = Palette::of(theme);
    let fill = button_fill(&p);
    let (bg, shadow) = match status {
        button::Status::Disabled => (alpha(p.muted, 0.18), Color::TRANSPARENT),
        button::Status::Hovered => (blend(fill, Color::BLACK, 0.12), alpha(fill, 0.25)),
        button::Status::Pressed => (blend(fill, Color::BLACK, 0.18), alpha(fill, 0.25)),
        _ => (fill, alpha(fill, 0.25)),
    };
    button::Style {
        background: Some(bg.into()),
        text_color: if status == button::Status::Disabled { alpha(p.text, 0.6) } else { readable_on(bg) },
        border: Border { radius: 8.0.into(), ..Default::default() },
        shadow: Shadow { color: shadow, offset: Vector::new(0.0, 2.0), blur_radius: 10.0 },
        ..Default::default()
    }
}

/// Black or white text, whichever reads better on  (WCAG relative luminance).
pub fn readable_on(bg: Color) -> Color {
    let lum = luminance(bg);
    // Black text has contrast (lum + 0.05) / 0.05; white has 1.05 / (lum + 0.05). Pick the larger.
    if (lum + 0.05) / 0.05 >= 1.05 / (lum + 0.05) { rgb(0x0A0A0B) } else { rgb(0xFFFFFF) }
}

pub fn danger(theme: &Theme, status: button::Status) -> button::Style {
    let p = Palette::of(theme);
    let (bg, text_color) = match status {
        button::Status::Disabled => (alpha(p.muted, 0.3), p.muted),
        button::Status::Hovered => {
            let c = blend(p.danger, p.text, 0.1);
            (c, readable_on(c))
        }
        button::Status::Pressed => {
            let c = blend(p.danger, Color::BLACK, 0.15);
            (c, readable_on(c))
        }
        _ => (p.danger, readable_on(p.danger)),
    };
    button::Style {
        background: Some(bg.into()),
        text_color,
        border: Border { radius: 8.0.into(), ..Default::default() },
        ..Default::default()
    }
}

/// Inputs sit on the card colour with a border that lights up on focus.
pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let p = Palette::of(theme);
    let focus = if is_light(&p) { button_fill(&p) } else { p.accent };
    let (edge, width) = match status {
        text_input::Status::Focused { .. } => (focus, 2.0),
        text_input::Status::Hovered => (p.muted, 1.0),
        text_input::Status::Disabled => (p.border, 1.0),
        _ => (p.border_strong, 1.0),
    };
    let disabled = status == text_input::Status::Disabled;
    text_input::Style {
        background: if disabled { p.surface } else { p.card }.into(),
        border: Border { color: edge, width, radius: 8.0.into() },
        icon: p.muted,
        placeholder: if disabled { alpha(p.muted, 0.8) } else { p.muted },
        value: if disabled { alpha(p.text, 0.6) } else { p.text },
        selection: alpha(p.accent, 0.35),
    }
}

/// A multi-line editor styled like [`input`].
pub fn editor(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let p = Palette::of(theme);
    let focus = if is_light(&p) { button_fill(&p) } else { p.accent };
    let (edge, width) = match status {
        text_editor::Status::Focused { .. } => (focus, 2.0),
        text_editor::Status::Hovered => (p.muted, 1.0),
        text_editor::Status::Disabled => (p.border, 1.0),
        text_editor::Status::Active => (p.border_strong, 1.0),
    };
    let disabled = status == text_editor::Status::Disabled;
    text_editor::Style {
        background: if disabled { p.surface } else { p.card }.into(),
        border: Border { color: edge, width, radius: 8.0.into() },
        placeholder: if disabled { alpha(p.muted, 0.8) } else { p.muted },
        value: if disabled { alpha(p.text, 0.6) } else { p.text },
        selection: alpha(p.accent, 0.35),
    }
}

/// Dialog checkbox: a filled box when checked, a 3:1 edge when not.
pub fn checkbox(theme: &Theme, status: checkbox_w::Status) -> checkbox_w::Style {
    let p = Palette::of(theme);
    let fill = button_fill(&p);
    let (checked, hovered, disabled) = match status {
        checkbox_w::Status::Active { is_checked } => (is_checked, false, false),
        checkbox_w::Status::Hovered { is_checked } => (is_checked, true, false),
        checkbox_w::Status::Disabled { is_checked } => (is_checked, false, true),
    };
    let (background, edge) = match (checked, disabled) {
        (true, true) => (alpha(fill, 0.55), alpha(fill, 0.55)),
        (true, false) => (fill, if hovered { p.accent } else { fill }),
        (false, true) => (p.raised, alpha(p.muted, 0.7)),
        (false, false) => (p.card, if hovered { p.accent } else { p.border_strong }),
    };
    checkbox_w::Style {
        background: background.into(),
        icon_color: Color::WHITE,
        border: Border { color: edge, width: 1.5, radius: 4.0.into() },
        text_color: Some(if disabled { alpha(p.muted, 0.85) } else { p.text }),
    }
}

/// Drop-down field.
pub fn pick(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let p = Palette::of(theme);
    let lit = matches!(status, pick_list::Status::Hovered | pick_list::Status::Opened { .. });
    pick_list::Style {
        text_color: p.text,
        placeholder_color: p.muted,
        handle_color: p.muted,
        background: p.card.into(),
        border: Border { color: if lit { p.accent } else { p.border }, width: 1.0, radius: 8.0.into() },
    }
}

/// The list a [`pick`] opens.
pub fn pick_menu(theme: &Theme) -> menu::Style {
    let p = Palette::of(theme);
    menu::Style {
        background: p.card.into(),
        border: Border { color: p.border, width: 1.0, radius: 8.0.into() },
        text_color: p.text,
        selected_text_color: p.text,
        selected_background: p.raised.into(),
        shadow: Shadow { color: p.shadow, offset: Vector::new(0.0, 4.0), blur_radius: 12.0 },
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

/// The accent gradient bar. `shimmer` is a 0..1 phase for a running download:
/// a soft highlight sweeps across the filled part. `None` draws it still.
pub fn progress(colour: Option<Color>, shimmer: Option<f32>) -> impl Fn(&Theme) -> progress_bar::Style {
    move |theme| {
        let p = Palette::of(theme);
        let bar = match (colour, shimmer) {
            (Some(c), _) => c.into(),
            (None, None) if is_light(&p) => Background::Gradient(Gradient::Linear(
                Linear::new(Radians(std::f32::consts::FRAC_PI_2))
                    .add_stop(0.0, button_fill(&p))
                    .add_stop(1.0, p.accent),
            )),
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
            background: p.border.into(),
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
            let r = ratio(on_button(&p), fill);
            assert!(r >= 4.5, "{name}: button text on the button fill is only {r:.2}:1");
        }
    }

    #[test]
    fn border_strong_is_visible_on_cards() {
        for (name, p) in [("dark", Palette::dark()), ("light", Palette::light())] {
            let r = ratio(p.border_strong, p.card);
            assert!(r >= 3.0, "{name}: border_strong on card is only {r:.2}:1");
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
