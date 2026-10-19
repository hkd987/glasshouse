//! The sign-in screen (the Sign in artboard): wallpaper, a large clock, the
//! user, a password field, and the device's status along the bottom.

use iced_core::alignment::{Horizontal, Vertical};
use iced_core::{Background, Border, Color, Length, Padding, border};
use iced_widget::{Row, button, column, container, image, row, stack, text, text_input};

use crate::theme::{hex, hexa};
use crate::{Element, Message, Theme, fonts, icons, style, wallpaper};

const INK: Color = hex(0xF5F7FA);

/// What the sign-in screen shows. The clock strings come from the caller,
/// in the user's locale.
#[derive(Debug, Clone, PartialEq)]
pub struct SignIn {
    pub date: String,
    pub time: String,
    pub user: String,
    pub initials: String,
    /// "[Org] managed device · signs in with Entra ID".
    pub managed_by: String,
    /// None on a machine without a battery.
    pub battery_percent: Option<u8>,
    pub password: String,
    /// Why the last try didn't sign in, shown in place of the hint.
    pub message: Option<String>,
    /// Whether the hint offers the fingerprint sensor.
    pub fingerprint: bool,
    /// Whether "Switch user" is offered: on the sign-in screen, not the
    /// locked screen, which only its own person unlocks.
    pub switch_user: bool,
    /// False leaves the wallpaper out, for a screen that keeps it as its
    /// backdrop (`render::Screen::set_backdrop`).
    pub wallpaper: bool,
}

/// The id of the password field, to give it the keyboard focus.
pub const PASSWORD: &str = "sign-in-password";

pub fn view<'a>(theme: &Theme, screen: &SignIn, size: (f32, f32)) -> Element<'a, Message> {
    let glass = |alpha: f32| hexa(0xF5F7FA, alpha);
    let edge = hexa(0xFFFFFF, 0.25);
    let accent = theme.accent;

    let clock = column![
        text(screen.date.clone())
            .size(22)
            .font(fonts::MEDIUM)
            .color(Color { a: 0.85, ..INK }),
        text(screen.time.clone())
            .size(112)
            .font(fonts::SEMIBOLD)
            .color(INK)
            .line_height(1.0),
    ]
    .spacing(4)
    .align_x(Horizontal::Center);

    let avatar = container(
        text(screen.initials.clone())
            .size(26)
            .font(fonts::MEDIUM)
            .color(INK),
    )
    .center(Length::Fixed(76.0))
    .style(move |_| style::outlined(glass(0.22), 38.0, 1.0, edge));

    let field = text_input("Enter password", &screen.password)
        .id(PASSWORD)
        .secure(true)
        .on_input(Message::PasswordChanged)
        .on_submit(Message::SignIn)
        .size(14)
        .font(fonts::SANS)
        .padding(Padding {
            top: 11.0,
            right: 44.0,
            bottom: 11.0,
            left: 18.0,
        })
        .width(Length::Fixed(240.0))
        .style(move |_, _| text_input::Style {
            background: Background::Color(glass(0.2)),
            border: Border {
                radius: border::Radius::from(20.0),
                width: 1.0,
                color: edge,
            },
            icon: INK,
            placeholder: Color { a: 0.6, ..INK },
            value: INK,
            selection: Color { a: 0.4, ..accent },
        });
    let submit = button(icons::view(&icons::ARROW_RIGHT, INK, 14.0, 14.0))
        .on_press(Message::SignIn)
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(32.0))
        .padding(9)
        .style(style::button_like(style::panel(glass(0.28), 16.0)));
    let password = stack![
        field,
        container(submit)
            .width(Length::Fixed(240.0))
            .height(Length::Fixed(40.0))
            .align_x(Horizontal::Right)
            .align_y(Vertical::Center)
            .padding(Padding {
                right: 4.0,
                ..Padding::ZERO
            }),
    ];

    let user = column![
        avatar,
        text(screen.user.clone())
            .size(18)
            .font(fonts::SEMIBOLD)
            .color(INK),
        password,
        match &screen.message {
            Some(message) => text(message.clone()).size(13).color(hex(0xFFD7A8)),
            None => text(if screen.fingerprint {
                "Touch the fingerprint sensor or enter your password"
            } else {
                "Enter your password"
            })
            .size(13)
            .color(Color { a: 0.8, ..INK }),
        },
    ]
    .spacing(14)
    .align_x(Horizontal::Center);

    let mut status = Row::new();
    if screen.switch_user {
        status = status.push(
            button(text("Switch user").size(13).color(INK))
                .on_press(Message::SwitchUser)
                .padding(Padding::from([7.0, 12.0]))
                .style(style::button_like(style::panel(glass(0.16), 16.0))),
        );
    }
    status = status
        .push(icons::view(&icons::ACCESSIBILITY, INK, 20.0, 20.0))
        .push(icons::view(&icons::WIFI, INK, 20.0, 20.0));
    if let Some(percent) = screen.battery_percent {
        status = status.push(
            row![
                icons::view(&icons::BATTERY, INK, 26.0, 14.0),
                text(format!("{percent}%")).size(13).color(INK),
            ]
            .spacing(6)
            .align_y(Vertical::Center),
        );
    }
    let status = status.spacing(18).align_y(Vertical::Center);
    let bottom = row![
        text(screen.managed_by.clone())
            .size(13)
            .color(Color { a: 0.75, ..INK }),
        iced_widget::space().width(Length::Fill),
        status,
    ]
    .align_y(Vertical::Center);

    let backdrop: Element<'a, Message> = if screen.wallpaper {
        image(wallpaper::handle(&theme.wallpaper, size.0, size.1))
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(iced_core::ContentFit::Fill)
            .into()
    } else {
        iced_widget::space()
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };
    stack![
        backdrop,
        container(clock)
            .width(Length::Fill)
            .align_x(Horizontal::Center)
            .padding(Padding {
                top: 96.0,
                ..Padding::ZERO
            }),
        container(user)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Bottom)
            .padding(Padding {
                bottom: 120.0,
                ..Padding::ZERO
            }),
        container(bottom)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(Vertical::Bottom)
            .padding(Padding {
                left: 32.0,
                right: 32.0,
                bottom: 24.0,
                top: 0.0,
            }),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
