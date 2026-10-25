//! The brightness keys' level (ADR 0060): a small panel low on the screen
//! for a moment after each press, as a Mac shows it.

use iced_core::alignment::{Horizontal, Vertical};
use iced_core::{Color, Length, Padding};
use iced_widget::{column, container, row, space, text};

use crate::theme::hexa;
use crate::{Element, Message, fonts, icons, style};

/// The bar's width, in logical pixels.
const BAR: f32 = 150.0;

pub fn overlay<'a>(percent: u8) -> Element<'a, Message> {
    let lit = BAR * f32::from(percent.min(100)) / 100.0;
    let segment = move |width: f32, color: Color| {
        container(space())
            .width(Length::Fixed(width))
            .height(Length::Fixed(6.0))
            .style(move |_| style::panel(color, 3.0))
    };
    let bar = row![
        segment(lit, Color::WHITE),
        segment(BAR - lit, hexa(0xFFFFFF, 0.22))
    ];
    let panel = container(
        column![
            icons::view(&icons::SUN, Color::WHITE, 36.0, 36.0),
            bar,
            text(format!("{percent}%"))
                .size(13)
                .font(fonts::MEDIUM)
                .color(hexa(0xFFFFFF, 0.8)),
        ]
        .spacing(12)
        .align_x(Horizontal::Center),
    )
    .width(Length::Fixed(BAR + 40.0))
    .padding(20)
    .style(|_| style::panel(hexa(0x15181D, 0.94), 18.0));
    container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Bottom)
        .padding(Padding {
            bottom: 120.0,
            ..Padding::ZERO
        })
        .into()
}
