//! The boot screen: the mark on black, and a progress bar while the system
//! comes up (the Boot artboard).

use iced_core::alignment::Horizontal;
use iced_core::{Color, Length};
use iced_widget::{column, container};

use crate::theme::hex;
use crate::{Element, Message, Theme, icons, style};

const INK: Color = hex(0xF5F7FA);
const TRACK: Color = hex(0x3A3D42);
const BAR_WIDTH: f32 = 200.0;

/// `progress` is 0.0 to 1.0.
pub fn view<'a>(_theme: &Theme, progress: f32) -> Element<'a, Message> {
    let fill = (BAR_WIDTH * progress.clamp(0.0, 1.0)).max(5.0);
    let bar = container(
        container("")
            .width(Length::Fixed(fill))
            .height(Length::Fixed(5.0))
            .style(move |_| style::panel(INK, 2.5)),
    )
    .width(Length::Fixed(BAR_WIDTH))
    .height(Length::Fixed(5.0))
    .style(|_| style::panel(TRACK, 2.5));

    container(
        column![icons::view(&icons::MARK, INK, 104.0, 104.0), bar]
            .spacing(64)
            .align_x(Horizontal::Center),
    )
    .center(Length::Fill)
    .style(|_| style::panel(Color::BLACK, 0.0))
    .into()
}
