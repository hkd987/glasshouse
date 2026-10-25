//! evdev key codes to iced keys and the text they type, for a US keyboard.
//! The compositor replaces this with xkbcommon and the user's layout
//! (ADR 0016).

use iced_core::keyboard::key::{Named, NativeCode, Physical};
use iced_core::keyboard::{Key, Location, Modifiers};

/// What a key is: a named key, or one that types a character (without and
/// with Shift).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Named(Named),
    Char(char, char),
    Modifier(Modifiers, Named),
}

fn kind(code: u16) -> Option<Kind> {
    use Kind::{Char, Modifier};
    let named = |n| Some(Kind::Named(n));
    // Linux input-event-codes.h numbers, which evdev on FreeBSD shares.
    match code {
        1 => named(Named::Escape),
        2..=11 => {
            let i = usize::from(code - 2);
            Some(Char(b"1234567890"[i] as char, b"!@#$%^&*()"[i] as char))
        }
        12 => Some(Char('-', '_')),
        13 => Some(Char('=', '+')),
        14 => named(Named::Backspace),
        15 => named(Named::Tab),
        16..=25 => Some(letter(b"qwertyuiop"[usize::from(code - 16)])),
        26 => Some(Char('[', '{')),
        27 => Some(Char(']', '}')),
        28 | 96 => named(Named::Enter),
        29 | 97 => Some(Modifier(Modifiers::CTRL, Named::Control)),
        30..=38 => Some(letter(b"asdfghjkl"[usize::from(code - 30)])),
        39 => Some(Char(';', ':')),
        40 => Some(Char('\'', '"')),
        41 => Some(Char('`', '~')),
        42 | 54 => Some(Modifier(Modifiers::SHIFT, Named::Shift)),
        43 => Some(Char('\\', '|')),
        44..=50 => Some(letter(b"zxcvbnm"[usize::from(code - 44)])),
        51 => Some(Char(',', '<')),
        52 => Some(Char('.', '>')),
        53 => Some(Char('/', '?')),
        56 | 100 => Some(Modifier(Modifiers::ALT, Named::Alt)),
        57 => named(Named::Space),
        59..=68 => named(
            [
                Named::F1,
                Named::F2,
                Named::F3,
                Named::F4,
                Named::F5,
                Named::F6,
                Named::F7,
                Named::F8,
                Named::F9,
                Named::F10,
            ][usize::from(code - 59)],
        ),
        87 => named(Named::F11),
        88 => named(Named::F12),
        102 => named(Named::Home),
        103 => named(Named::ArrowUp),
        104 => named(Named::PageUp),
        105 => named(Named::ArrowLeft),
        106 => named(Named::ArrowRight),
        107 => named(Named::End),
        108 => named(Named::ArrowDown),
        109 => named(Named::PageDown),
        111 => named(Named::Delete),
        125 | 126 => Some(Modifier(Modifiers::LOGO, Named::Super)),
        // KEY_BRIGHTNESSDOWN and KEY_BRIGHTNESSUP (ADR 0060).
        224 => named(Named::BrightnessDown),
        225 => named(Named::BrightnessUp),
        _ => None,
    }
}

fn letter(lower: u8) -> Kind {
    Kind::Char(lower as char, lower.to_ascii_uppercase() as char)
}

/// The keyboard state: which modifiers are held.
#[derive(Debug, Default)]
pub struct Keyboard {
    modifiers: Modifiers,
}

/// One key going down or up, as iced sees it.
pub struct Press {
    pub key: Key,
    pub modifiers: Modifiers,
    pub event: iced_core::keyboard::Event,
}

impl Keyboard {
    /// The events for evdev key `code` with `value` (1 down, 0 up, 2
    /// repeat), or none for keys this map doesn't know.
    pub fn key(&mut self, code: u16, value: i32) -> Vec<Press> {
        let Some(kind) = kind(code) else {
            return Vec::new();
        };
        let down = value != 0;
        let mut presses = Vec::new();
        let (key, text) = match kind {
            Kind::Named(named) => (
                Key::Named(named),
                (named == Named::Space).then(|| " ".to_owned()),
            ),
            Kind::Char(plain, shifted) => {
                let c = if self.modifiers.shift() {
                    shifted
                } else {
                    plain
                };
                (Key::Character(c.to_string().into()), Some(c.to_string()))
            }
            Kind::Modifier(modifier, named) => {
                self.modifiers.set(modifier, down);
                presses.push(Press {
                    key: Key::Named(named),
                    modifiers: self.modifiers,
                    event: iced_core::keyboard::Event::ModifiersChanged(self.modifiers),
                });
                (Key::Named(named), None)
            }
        };
        // evdev codes are the Linux keycodes, which X numbers 8 higher.
        let physical = Physical::Unidentified(NativeCode::Xkb(u32::from(code) + 8));
        let modifiers = self.modifiers;
        let event = if down {
            iced_core::keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key.clone(),
                physical_key: physical,
                location: Location::Standard,
                modifiers,
                // Ctrl and the logo key make shortcuts, not text.
                text: text
                    .filter(|_| !modifiers.control() && !modifiers.logo())
                    .map(Into::into),
                repeat: value == 2,
            }
        } else {
            iced_core::keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key.clone(),
                physical_key: physical,
                location: Location::Standard,
                modifiers,
            }
        };
        presses.push(Press {
            key,
            modifiers,
            event,
        });
        presses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(keyboard: &mut Keyboard, code: u16) -> Option<String> {
        keyboard
            .key(code, 1)
            .into_iter()
            .find_map(|p| match p.event {
                iced_core::keyboard::Event::KeyPressed { text, .. } => text.map(|t| t.to_string()),
                _ => None,
            })
    }

    #[test]
    fn letters_digits_and_shift_type_what_a_us_keyboard_types() {
        let mut keyboard = Keyboard::default();
        assert_eq!(typed(&mut keyboard, 35).as_deref(), Some("h"));
        assert_eq!(typed(&mut keyboard, 3).as_deref(), Some("2"));
        assert_eq!(typed(&mut keyboard, 57).as_deref(), Some(" "));
        keyboard.key(42, 1);
        assert_eq!(typed(&mut keyboard, 35).as_deref(), Some("H"));
        assert_eq!(typed(&mut keyboard, 3).as_deref(), Some("@"));
        keyboard.key(42, 0);
        assert_eq!(typed(&mut keyboard, 52).as_deref(), Some("."));
    }

    #[test]
    fn ux20_function_keys_are_named_so_ctrl_f2_reaches_the_shell() {
        let mut keyboard = Keyboard::default();
        keyboard.key(29, 1);
        let f2 = keyboard.key(60, 1).pop().expect("a press");
        assert_eq!(f2.key, Key::Named(Named::F2));
        assert!(f2.modifiers.control());
        let f12 = keyboard.key(88, 1).pop().expect("a press");
        assert_eq!(f12.key, Key::Named(Named::F12));
        assert_eq!(typed(&mut keyboard, 59), None, "F1 types nothing");
    }

    #[test]
    fn ctrl_space_is_a_shortcut_not_text() {
        let mut keyboard = Keyboard::default();
        let ctrl = keyboard.key(29, 1);
        assert!(matches!(
            ctrl[0].event,
            iced_core::keyboard::Event::ModifiersChanged(m) if m.control()
        ));
        let space = keyboard.key(57, 1).pop().expect("a press");
        assert_eq!(space.key, Key::Named(Named::Space));
        assert!(space.modifiers.control());
        assert_eq!(typed(&mut keyboard, 57), None);
    }
}
