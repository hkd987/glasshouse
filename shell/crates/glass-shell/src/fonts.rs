//! The shell's typefaces, bundled so every machine (and every snapshot)
//! renders the same text: Geist for the interface and Geist Mono for agent
//! feeds and numbers. Both are under the SIL Open Font License 1.1
//! (`shell/assets/fonts/OFL-*.txt`).

use std::borrow::Cow;
use std::sync::Once;

use iced_core::Font;
use iced_core::font::{Family, Weight};

const GEIST_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Geist-Regular.ttf");
const GEIST_MEDIUM: &[u8] = include_bytes!("../../../assets/fonts/Geist-Medium.ttf");
const GEIST_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Geist-SemiBold.ttf");
const GEIST_MONO: &[u8] = include_bytes!("../../../assets/fonts/GeistMono-Regular.ttf");

pub const SANS: Font = Font::with_name("Geist");
pub const MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..SANS
};
pub const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..SANS
};
pub const MONO: Font = Font {
    family: Family::Name("Geist Mono"),
    ..Font::DEFAULT
};

/// Registers the fonts with the text engine. Safe to call more than once.
pub fn load() {
    static LOADED: Once = Once::new();
    LOADED.call_once(|| {
        let mut fonts = iced_renderer::graphics::text::font_system()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for bytes in [GEIST_REGULAR, GEIST_MEDIUM, GEIST_SEMIBOLD, GEIST_MONO] {
            fonts.load_font(Cow::Borrowed(bytes));
        }
    });
}
