//! The keyboard and pointer, read from the kernel's evdev devices on one
//! thread each and handed to the shell's loop as [`Input`].

use std::thread;

use glass_platform::input::{InputDevice, RawEvent, codes, devices};
use tokio::sync::mpsc;

/// What the person did, in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// The pointer is at (x, y): a tablet or touch screen.
    MovedTo(f32, f32),
    /// The pointer moved by (dx, dy): a mouse.
    MovedBy(f32, f32),
    Button(iced_core::mouse::Button, bool),
    /// Wheel clicks, up positive.
    Wheel(f32),
    /// An evdev key code and its value: 1 down, 0 up, 2 repeat.
    Key(u16, i32),
}

/// Opens every input device, takes it from the console, and reads it on its
/// own thread. Returns the stream and how many devices it reads.
pub fn start(screen: (u32, u32)) -> (mpsc::UnboundedReceiver<Input>, usize) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut opened = 0;
    for path in devices().unwrap_or_default() {
        let mut device = match InputDevice::open(&path) {
            Ok(device) => device,
            Err(e) => {
                eprintln!("input: {e}");
                continue;
            }
        };
        let name = device.name().unwrap_or_default();
        if let Err(e) = device.grab() {
            eprintln!("input: {} ({name}): can't take it: {e}", path.display());
        }
        let axes = device
            .abs_range(codes::ABS_X)
            .and_then(|x| Ok((x, device.abs_range(codes::ABS_Y)?)))
            .ok();
        eprintln!("input: {} ({name})", path.display());
        let tx = tx.clone();
        opened += 1;
        thread::spawn(move || {
            let mut batch = Batch::new(axes, screen);
            while let Ok(events) = device.read() {
                for input in events.iter().filter_map(|e| batch.push(*e)) {
                    if tx.send(input).is_err() {
                        return;
                    }
                }
            }
        });
    }
    (rx, opened)
}

type Axes = (std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>);

/// One device's events between two `SYN_REPORT`s: an absolute X and Y, or
/// relative motion, arrive separately and count as one move.
struct Batch {
    axes: Option<Axes>,
    screen: (u32, u32),
    at: (Option<i32>, Option<i32>),
    by: (i32, i32),
}

impl Batch {
    fn new(axes: Option<Axes>, screen: (u32, u32)) -> Self {
        Self {
            axes,
            screen,
            at: (None, None),
            by: (0, 0),
        }
    }

    fn push(&mut self, event: RawEvent) -> Option<Input> {
        match (event.kind, event.code) {
            (codes::EV_ABS, codes::ABS_X) => self.at.0 = Some(event.value),
            (codes::EV_ABS, codes::ABS_Y) => self.at.1 = Some(event.value),
            (codes::EV_REL, codes::REL_X) => self.by.0 += event.value,
            (codes::EV_REL, codes::REL_Y) => self.by.1 += event.value,
            (codes::EV_REL, codes::REL_WHEEL) => return Some(Input::Wheel(event.value as f32)),
            (codes::EV_KEY, code) => {
                let button = match code {
                    codes::BTN_LEFT => iced_core::mouse::Button::Left,
                    codes::BTN_RIGHT => iced_core::mouse::Button::Right,
                    codes::BTN_MIDDLE => iced_core::mouse::Button::Middle,
                    _ => return Some(Input::Key(code, event.value)),
                };
                return (event.value != 2).then_some(Input::Button(button, event.value == 1));
            }
            (codes::EV_SYN, codes::SYN_REPORT) => return self.flush(),
            _ => {}
        }
        None
    }

    fn flush(&mut self) -> Option<Input> {
        let by = std::mem::take(&mut self.by);
        let at = std::mem::take(&mut self.at);
        if let (Some((xs, ys)), (Some(x), Some(y))) = (&self.axes, at) {
            return Some(Input::MovedTo(
                scale(x, xs, self.screen.0),
                scale(y, ys, self.screen.1),
            ));
        }
        (by != (0, 0)).then_some(Input::MovedBy(by.0 as f32, by.1 as f32))
    }
}

/// An absolute axis value as a screen pixel.
fn scale(value: i32, range: &std::ops::RangeInclusive<i32>, pixels: u32) -> f32 {
    let span = (range.end() - range.start()).max(1) as f32;
    ((value - range.start()) as f32 / span * (pixels.saturating_sub(1)) as f32)
        .clamp(0.0, pixels as f32 - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: u16, code: u16, value: i32) -> RawEvent {
        RawEvent { kind, code, value }
    }

    #[test]
    fn a_tablets_x_and_y_make_one_move_in_screen_pixels() {
        let mut batch = Batch::new(Some((0..=32767, 0..=32767)), (1281, 801));
        assert_eq!(batch.push(ev(codes::EV_ABS, codes::ABS_X, 16_384)), None);
        assert_eq!(batch.push(ev(codes::EV_ABS, codes::ABS_Y, 32_767)), None);
        let moved = batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0));
        assert!(
            matches!(moved, Some(Input::MovedTo(x, y)) if (x - 640.0).abs() < 0.1 && y == 800.0)
        );
        // A button alone moves nothing.
        assert_eq!(
            batch.push(ev(codes::EV_KEY, codes::BTN_LEFT, 1)),
            Some(Input::Button(iced_core::mouse::Button::Left, true))
        );
        assert_eq!(batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0)), None);
    }

    #[test]
    fn a_mouses_motion_adds_up_and_keys_pass_through() {
        let mut batch = Batch::new(None, (1280, 800));
        batch.push(ev(codes::EV_REL, codes::REL_X, 3));
        batch.push(ev(codes::EV_REL, codes::REL_X, 2));
        assert_eq!(
            batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0)),
            Some(Input::MovedBy(5.0, 0.0))
        );
        assert_eq!(
            batch.push(ev(codes::EV_KEY, 30, 1)),
            Some(Input::Key(30, 1))
        );
    }
}
