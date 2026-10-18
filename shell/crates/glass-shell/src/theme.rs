//! Design tokens. One [`Theme`] restyles everything at once: menu bar,
//! dock, windows, agent tiles and wallpaper (UX-10). [`Theme::glass`] is
//! the default from the mockups; IT's brand theme and the others (Graphite,
//! Paper, Midnight, High contrast) are more values of the same struct.

use iced_core::Color;

/// A color from `0xRRGGBB`.
pub const fn hex(rgb: u32) -> Color {
    Color {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}

/// A color from `0xRRGGBB` and an opacity.
pub const fn hexa(rgb: u32, alpha: f32) -> Color {
    let c = hex(rgb);
    Color { a: alpha, ..c }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    /// Working tiles, focus rings, the agents pill, selection.
    pub accent: Color,
    /// Needs you: the only color that pulses.
    pub attention: Color,
    pub wallpaper: Wallpaper,
    pub menu_bar: Surface,
    pub dock: Surface,
    pub window: WindowColors,
    pub tile: TileColors,
}

/// A translucent bar or panel over the wallpaper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    pub background: Color,
    pub text: Color,
    /// Faint fills inside it, such as the agents pill.
    pub fill: Color,
    pub border: Color,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowColors {
    pub background: Color,
    pub title_bar: Color,
    pub border: Color,
    pub divider: Color,
    pub text: Color,
    pub muted: Color,
    /// The window controls, top left.
    pub controls: Color,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileColors {
    pub background: Color,
    pub text: Color,
    /// Secondary text: details, elapsed time, counters.
    pub muted: Color,
    /// The caption under a tile's feed or thumbnail.
    pub caption: Color,
    pub feed_background: Color,
    pub feed_text: Color,
    /// The newest feed line's verb.
    pub feed_verb: Color,
    pub chip_border: Color,
    pub button_border: Color,
    pub track: Color,
    pub outline: Color,
}

/// The wallpaper: a base color under soft blurred shapes.
#[derive(Debug, Clone, PartialEq)]
pub struct Wallpaper {
    pub base: Color,
    pub blobs: Vec<Blob>,
    /// Gaussian blur radius, in logical pixels.
    pub blur: f32,
}

/// One rounded, rotated shape of the wallpaper, in logical pixels on a
/// 1440x900 screen (scaled to other sizes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blob {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub radius: f32,
    pub degrees: f32,
    pub color: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::glass()
    }
}

impl Theme {
    /// "Glass", the default theme in the mockups.
    pub fn glass() -> Self {
        let blob = |x, y, width, height, degrees, color| Blob {
            x,
            y,
            width,
            height,
            radius: 80.0,
            degrees,
            color,
        };
        Self {
            name: "Glass",
            accent: hex(0x3D6BFF),
            attention: hex(0xF08A24),
            wallpaper: Wallpaper {
                base: hex(0x0E1626),
                blobs: vec![
                    blob(-120.0, 320.0, 760.0, 520.0, -14.0, hexa(0x2F55D4, 0.55)),
                    blob(520.0, 420.0, 640.0, 460.0, 9.0, hexa(0x0E8A8A, 0.45)),
                    blob(980.0, 120.0, 620.0, 520.0, -8.0, hexa(0x6E56E8, 0.45)),
                    blob(300.0, -140.0, 700.0, 360.0, 6.0, hexa(0x1E3A6E, 0.7)),
                ],
                blur: 48.0,
            },
            menu_bar: Surface {
                background: hexa(0xF5F7FA, 0.8),
                text: hex(0x14171C),
                fill: hexa(0x14171C, 0.08),
                border: hexa(0x14171C, 0.0),
            },
            dock: Surface {
                background: hexa(0xF5F7FA, 0.55),
                text: hex(0x14171C),
                fill: hexa(0x14171C, 0.2),
                border: hexa(0xFFFFFF, 0.45),
            },
            window: WindowColors {
                background: hex(0xFFFFFF),
                title_bar: hex(0xF4F6F8),
                border: hex(0xC9D0D8),
                divider: hex(0xE1E5EA),
                text: hex(0x14171C),
                muted: hex(0x5F6875),
                controls: hex(0xC3C9D0),
            },
            tile: TileColors {
                background: hex(0x15181D),
                text: hex(0xF3F5F7),
                muted: hex(0xA9B1BC),
                caption: hex(0xD6DBE1),
                feed_background: hex(0x0E1013),
                feed_text: hex(0x8C95A1),
                feed_verb: hex(0x9DB4FF),
                chip_border: hex(0x343A43),
                button_border: hex(0x3A414B),
                track: hex(0x2A3038),
                outline: hex(0x2A3038),
            },
        }
    }
}
