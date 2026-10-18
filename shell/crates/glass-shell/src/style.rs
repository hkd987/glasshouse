//! Small styling helpers shared by the screens.

use iced_core::{Background, Border, Color, Shadow, Vector, border};
use iced_widget::{button, container};

/// A filled, rounded panel.
pub fn panel(background: Color, radius: f32) -> container::Style {
    container::Style {
        background: Some(Background::Color(background)),
        border: Border {
            radius: border::Radius::from(radius),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// [`panel`] with an outline.
pub fn outlined(background: Color, radius: f32, width: f32, color: Color) -> container::Style {
    container::Style {
        border: Border {
            radius: border::Radius::from(radius),
            width,
            color,
        },
        ..panel(background, radius)
    }
}

/// A soft drop shadow below a floating surface.
pub fn shadow(alpha: f32, y: f32, blur: f32) -> Shadow {
    Shadow {
        color: Color {
            a: alpha,
            ..Color::BLACK
        },
        offset: Vector::new(0.0, y),
        blur_radius: blur,
    }
}

/// A button that looks the same in every state (previews have no hover),
/// with its look given as a container style.
pub fn button_like(
    look: container::Style,
) -> impl Fn(&iced_widget::Theme, button::Status) -> button::Style {
    move |_, _| button::Style {
        background: look.background,
        text_color: look.text_color.unwrap_or(Color::BLACK),
        border: look.border,
        shadow: look.shadow,
        snap: true,
    }
}

/// A menu item: plain until the pointer is on it, then `highlight` (the
/// accent, as a Mac menu highlights the item under the pointer).
pub fn menu_item(
    text: Color,
    highlight: Color,
) -> impl Fn(&iced_widget::Theme, button::Status) -> button::Style {
    move |_, status| {
        let on = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: on.then_some(Background::Color(highlight)),
            text_color: if on { Color::WHITE } else { text },
            border: Border {
                radius: border::Radius::from(6.0),
                ..Border::default()
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// A circle of `diameter` in `color`.
pub fn dot(color: Color, diameter: f32) -> container::Style {
    panel(color, diameter / 2.0)
}
