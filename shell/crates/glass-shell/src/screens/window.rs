//! An app window's title bar, as the compositor draws it above the window
//! (DK-1): the controls on the left as on a Mac, red, yellow and green while
//! the window has the keyboard and grey otherwise, and the title centered.

use iced_core::alignment::{Horizontal, Vertical};
use iced_core::{Color, Length, Padding, border};
use iced_widget::{container, row, stack, text};

use crate::theme::hex;
use crate::{Element, Message, Theme, fonts, style};

/// Where the compositor expects the controls ([`title_bar`] draws them
/// there, and clicks on them land there).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Controls {
    /// The first control's center, from the bar's left edge.
    pub first: f32,
    /// From one control's center to the next.
    pub step: f32,
    pub radius: f32,
}

/// The title bar of a window `width` wide and `height` tall.
pub fn title_bar<'a>(
    theme: &Theme,
    title: &str,
    focused: bool,
    (width, height): (f32, f32),
    controls: Controls,
) -> Element<'a, Message> {
    let w = theme.window;
    let colors: [Color; 3] = if focused {
        [hex(0xFF5F57), hex(0xFEBC2E), hex(0x28C840)]
    } else {
        [w.controls; 3]
    };
    let size = controls.radius * 2.0;
    let dot = move |color: Color| {
        container("")
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(move |_| style::dot(color, size))
    };
    let dots = row![dot(colors[0]), dot(colors[1]), dot(colors[2])]
        .spacing(controls.step - size)
        .padding(Padding {
            left: controls.first - controls.radius,
            ..Padding::ZERO
        });
    let title_color = if focused { w.text } else { w.muted };
    container(
        stack![
            container(dots)
                .height(Length::Fill)
                .align_y(Vertical::Center),
            container(
                text(title.to_owned())
                    .size(13)
                    .font(fonts::MEDIUM)
                    .color(title_color)
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fixed(width))
    .height(Length::Fixed(height))
    .style(move |_| container::Style {
        background: Some(w.title_bar.into()),
        border: border::rounded(border::top(10.0)),
        ..container::Style::default()
    })
    .into()
}
