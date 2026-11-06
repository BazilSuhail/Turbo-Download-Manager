//! Reusable widgets for the shell and its dialogs. Every colour comes from a
//! [`Palette`]. Where text sits on a filled background, its colour comes from
//! [`readable_on`], so it stays legible whichever palette is active.

use iced::widget::{button, column, container, row, text, Space};
use iced::{Border, Element, Length, Shadow, Vector};

use crate::ui::theme::{self, readable_on, Palette};

/// Centred title over a muted hint, for a view with nothing to show yet.
/// No fill is drawn, so both lines use the palette's text colours directly.
pub fn empty_state<'a, M: 'a>(p: &Palette, title: &'a str, hint: &'a str) -> Element<'a, M> {
    let (title_color, hint_color) = (p.text, p.muted);
    column![
        container(text(title).size(16).style(move |_| text::Style { color: Some(title_color) }))
            .center_x(Length::Fill),
        container(text(hint).size(13).style(move |_| text::Style { color: Some(hint_color) }))
            .center_x(Length::Fill),
    ]
    .spacing(6)
    .padding(24)
    .width(Length::Fill)
    .into()
}

/// A card with a title, the caller's body, and a right-aligned action row.
pub fn dialog_shell<'a, M: Clone + 'a>(
    p: &Palette,
    title: &'a str,
    body: Element<'a, M>,
    actions: Element<'a, M>,
) -> Element<'a, M> {
    let p = *p;
    let title_color = readable_on(p.card);
    let content = column![
        text(title).size(16).style(move |_| text::Style { color: Some(title_color) }),
        body,
        row![Space::new().width(Length::Fill), actions]
            .spacing(8)
            .align_y(iced::Alignment::Center),
    ]
    .spacing(14);
    container(content)
        .padding(16)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(p.card.into()),
            text_color: Some(p.text),
            border: Border { color: p.border, width: 1.0, radius: 12.0.into() },
            shadow: Shadow { color: p.shadow, offset: Vector::new(0.0, 6.0), blur_radius: 24.0 },
            ..Default::default()
        })
        .into()
}

/// A small card showing one figure: a muted label above the value.
pub fn stat_tile<'a, M: 'a>(p: &Palette, label: &'a str, value: String) -> Element<'a, M> {
    let p = *p;
    let value_color = readable_on(p.card);
    container(
        column![
            text(label).size(12).style(move |_| text::Style { color: Some(p.muted) }),
            text(value).size(22).style(move |_| text::Style { color: Some(value_color) }),
        ]
        .spacing(4),
    )
    .padding(12)
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(p.card.into()),
        text_color: Some(p.text),
        border: Border { color: p.border, width: 1.0, radius: 10.0.into() },
        ..Default::default()
    })
    .into()
}

/// A section heading, drawn on the window background.
pub fn section_title<'a, M: 'a>(p: &Palette, heading: &'a str) -> Element<'a, M> {
    let color = p.text;
    text(heading).size(15).style(move |_| text::Style { color: Some(color) }).into()
}

/// The main call to action. Uses the accent button style, which sets its own
/// fill and text colour from the active theme, so `_p` is not needed here.
pub fn primary_button<'a, M: Clone + 'a>(_p: &Palette, label: &'a str, on_press: M) -> Element<'a, M> {
    button(text(label).size(13))
        .padding([7, 18])
        .style(theme::accent)
        .on_press(on_press)
        .into()
}

/// A bordered, raised button for secondary actions. The label colour is
/// chosen against the fill it sits on.
pub fn secondary_button<'a, M: Clone + 'a>(p: &Palette, label: &'a str, on_press: M) -> Element<'a, M> {
    let p = *p;
    button(text(label).size(13))
        .padding([7, 18])
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let disabled = matches!(status, button::Status::Disabled);
            let fill = if hovered { p.border } else { p.raised };
            let text_color = if disabled {
                iced::Color { a: 0.6, ..p.muted }
            } else {
                readable_on(fill)
            };
            let edge = if hovered { p.accent } else { p.border };
            button::Style {
                background: Some(fill.into()),
                text_color,
                border: Border { color: edge, width: 1.0, radius: 8.0.into() },
                ..Default::default()
            }
        })
        .on_press(on_press)
        .into()
}
