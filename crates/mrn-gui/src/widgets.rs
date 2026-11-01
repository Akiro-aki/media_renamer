//! Small shared view helpers.

use iced::widget::{container, text};
use iced::{Element, Theme};

/// Secondary (dimmed) small text.
pub fn dim<'a, M: 'a>(s: impl text::IntoFragment<'a>) -> Element<'a, M> {
    text(s).size(13).style(text::secondary).into()
}

/// A titled, bordered box.
pub fn card<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(content)
        .padding(12)
        .width(iced::Fill)
        .style(container::bordered_box)
        .into()
}

/// A small coloured label, e.g. a match score.
pub fn badge<'a, M: 'a>(s: impl text::IntoFragment<'a>, style: fn(&Theme) -> text::Style) -> Element<'a, M> {
    text(s).size(12).style(style).into()
}

/// Shorten to `max` characters with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Status colours readable on both light and dark themes (the stock
/// `text::success` green is too dark on dark backgrounds).
fn tone(theme: &Theme, dark: (u8, u8, u8), light: (u8, u8, u8)) -> text::Style {
    let (r, g, b) = if theme.extended_palette().is_dark { dark } else { light };
    text::Style {
        color: Some(iced::Color::from_rgb8(r, g, b)),
    }
}

pub fn ok(theme: &Theme) -> text::Style {
    tone(theme, (0x7d, 0xd8, 0x9f), (0x1b, 0x7a, 0x43))
}

pub fn warn(theme: &Theme) -> text::Style {
    tone(theme, (0xf2, 0xc0, 0x5c), (0x9a, 0x62, 0x00))
}

pub fn bad(theme: &Theme) -> text::Style {
    tone(theme, (0xff, 0x8a, 0x80), (0xb3, 0x26, 0x1e))
}
