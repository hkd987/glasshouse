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
                for input in events.iter().flat_map(|e| batch.push(*e)) {
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

/// One device's events between two `SYN_REPORT`s. A tablet reports only
/// the axes that changed, so its last X and Y are kept; a mouse's motion
/// adds up. Buttons and the wheel wait for the report's end, so a click
/// that came with a move acts where the move put the pointer.
struct Batch {
    axes: Option<Axes>,
    screen: (u32, u32),
    /// The tablet's position, as it last reported each axis.
    at: (Option<i32>, Option<i32>),
    /// The tablet moved in this report.
    moved: bool,
    by: (i32, i32),
    /// Buttons and wheel turns in this report, in order.
    waiting: Vec<Input>,
}

impl Batch {
    fn new(axes: Option<Axes>, screen: (u32, u32)) -> Self {
        Self {
            axes,
            screen,
            at: (None, None),
            moved: false,
            by: (0, 0),
            waiting: Vec::new(),
        }
    }

    /// What the event makes happen: nothing until the report ends, except
    /// keys, which stand alone.
    fn push(&mut self, event: RawEvent) -> Vec<Input> {
        match (event.kind, event.code) {
            (codes::EV_ABS, codes::ABS_X) => (self.at.0, self.moved) = (Some(event.value), true),
            (codes::EV_ABS, codes::ABS_Y) => (self.at.1, self.moved) = (Some(event.value), true),
            (codes::EV_REL, codes::REL_X) => self.by.0 += event.value,
            (codes::EV_REL, codes::REL_Y) => self.by.1 += event.value,
            (codes::EV_REL, codes::REL_WHEEL) => {
                self.waiting.push(Input::Wheel(event.value as f32));
            }
            (codes::EV_KEY, code) => {
                let button = match code {
                    codes::BTN_LEFT => iced_core::mouse::Button::Left,
                    codes::BTN_RIGHT => iced_core::mouse::Button::Right,
                    codes::BTN_MIDDLE => iced_core::mouse::Button::Middle,
                    _ => return vec![Input::Key(code, event.value)],
                };
                if event.value != 2 {
                    self.waiting.push(Input::Button(button, event.value == 1));
                }
            }
            (codes::EV_SYN, codes::SYN_REPORT) => return self.flush(),
            _ => {}
        }
        Vec::new()
    }

    /// The report's move first, then its buttons.
    fn flush(&mut self) -> Vec<Input> {
        let by = std::mem::take(&mut self.by);
        let moved = std::mem::take(&mut self.moved);
        let mut inputs = Vec::with_capacity(1 + self.waiting.len());
        if let (true, Some((xs, ys)), (Some(x), Some(y))) = (moved, &self.axes, self.at) {
            inputs.push(Input::MovedTo(
                scale(x, xs, self.screen.0),
                scale(y, ys, self.screen.1),
            ));
        } else if by != (0, 0) {
            inputs.push(Input::MovedBy(by.0 as f32, by.1 as f32));
        }
        inputs.append(&mut self.waiting);
        inputs
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
        assert!(
            batch
                .push(ev(codes::EV_ABS, codes::ABS_X, 16_384))
                .is_empty()
        );
        assert!(
            batch
                .push(ev(codes::EV_ABS, codes::ABS_Y, 32_767))
                .is_empty()
        );
        let moved = batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0));
        assert!(
            matches!(moved[..], [Input::MovedTo(x, y)] if (x - 640.0).abs() < 0.1 && y == 800.0)
        );
        // A button alone moves nothing.
        assert!(batch.push(ev(codes::EV_KEY, codes::BTN_LEFT, 1)).is_empty());
        assert_eq!(
            batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0)),
            [Input::Button(iced_core::mouse::Button::Left, true)]
        );
    }

    #[test]
    fn a_tablet_moving_along_one_axis_still_moves() {
        let mut batch = Batch::new(Some((0..=32767, 0..=32767)), (1281, 801));
        batch.push(ev(codes::EV_ABS, codes::ABS_X, 0));
        batch.push(ev(codes::EV_ABS, codes::ABS_Y, 0));
        batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0));
        // Only X changed: the report has no Y.
        batch.push(ev(codes::EV_ABS, codes::ABS_X, 32_767));
        assert_eq!(
            batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0)),
            [Input::MovedTo(1280.0, 0.0)]
        );
    }

    #[test]
    fn a_click_that_comes_with_a_move_lands_where_the_move_went() {
        let mut batch = Batch::new(Some((0..=32767, 0..=32767)), (1281, 801));
        batch.push(ev(codes::EV_ABS, codes::ABS_X, 0));
        batch.push(ev(codes::EV_ABS, codes::ABS_Y, 0));
        batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0));
        // The button can come before the axes in one report.
        batch.push(ev(codes::EV_KEY, codes::BTN_LEFT, 1));
        batch.push(ev(codes::EV_ABS, codes::ABS_X, 16_384));
        batch.push(ev(codes::EV_ABS, codes::ABS_Y, 16_384));
        let report = batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0));
        assert!(matches!(
            report[..],
            [
                Input::MovedTo(x, y),
                Input::Button(iced_core::mouse::Button::Left, true)
            ] if (x - 640.0).abs() < 0.1 && (y - 400.0).abs() < 0.1
        ));
    }

    #[test]
    fn a_mouses_motion_adds_up_and_keys_pass_through() {
        let mut batch = Batch::new(None, (1280, 800));
        batch.push(ev(codes::EV_REL, codes::REL_X, 3));
        batch.push(ev(codes::EV_REL, codes::REL_X, 2));
        assert_eq!(
            batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0)),
            [Input::MovedBy(5.0, 0.0)]
        );
        assert_eq!(batch.push(ev(codes::EV_KEY, 30, 1)), [Input::Key(30, 1)]);
    }
}
