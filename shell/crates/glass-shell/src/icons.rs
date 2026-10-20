//! Icons, from the mockups' own vector data, rasterized with tiny-skia and
//! shown as images. tiny-skia and kurbo (which parses SVG path data) are
//! already in the renderer, so this costs no SVG library.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use iced_core::image::Handle;
use iced_core::{Color, Length};
use iced_widget::image;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::Element;

/// Icons are drawn at twice their layout size and scaled by the renderer,
/// so they stay sharp on high-density screens.
const OVERSAMPLE: f32 = 2.0;

/// A vector icon: shapes in a view box, painted in one color unless a shape
/// names its own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Icon {
    pub view_box: (f32, f32),
    pub shapes: &'static [Shape],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// SVG path data.
    Path(&'static str, Ink),
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        ink: Ink,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        ink: Ink,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ink {
    Fill,
    Stroke(f32),
    /// A stroke with round ends.
    StrokeRound(f32),
    /// Filled with a fixed `0xRRGGBB`, whatever the icon's color.
    FillHex(u32),
    StrokeHex(u32, f32),
}

/// An icon as an image widget, `width` x `height` logical pixels.
pub fn view<'a, Message: 'a>(
    icon: &Icon,
    color: Color,
    width: f32,
    height: f32,
) -> Element<'a, Message> {
    image(handle(icon, color, width, height))
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .into()
}

/// An icon rasterized once per color and size: every frame rebuilds the
/// view, and the same image handle lets the renderer reuse its pixels too.
pub fn handle(icon: &Icon, color: Color, width: f32, height: f32) -> Handle {
    /// Enough for every icon on screen in a few colors; past it, start over.
    const MOST: usize = 256;
    type Key = (usize, usize, [u32; 2], [u32; 4], [u32; 2]);
    static CACHE: LazyLock<Mutex<HashMap<Key, Handle>>> = LazyLock::new(Mutex::default);
    // Icons are constants whose shapes live in static memory for good, so
    // where the shapes are tells icons apart.
    let key = (
        icon.shapes.as_ptr() as usize,
        icon.shapes.len(),
        [icon.view_box.0, icon.view_box.1].map(f32::to_bits),
        [color.r, color.g, color.b, color.a].map(f32::to_bits),
        [width, height].map(f32::to_bits),
    );
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = cache.get(&key) {
        return handle.clone();
    }
    if cache.len() >= MOST {
        cache.clear();
    }
    let handle = rasterize(icon, color, width, height);
    cache.insert(key, handle.clone());
    handle
}

fn rasterize(icon: &Icon, color: Color, width: f32, height: f32) -> Handle {
    let w = (width * OVERSAMPLE).ceil().max(1.0) as u32;
    let h = (height * OVERSAMPLE).ceil().max(1.0) as u32;
    let Some(mut pixmap) = Pixmap::new(w, h) else {
        return Handle::from_rgba(1, 1, vec![0; 4]);
    };
    let transform = Transform::from_scale(w as f32 / icon.view_box.0, h as f32 / icon.view_box.1);
    for shape in icon.shapes {
        let (path, ink) = match *shape {
            Shape::Path(d, ink) => (kurbo::BezPath::from_svg(d).ok(), ink),
            Shape::Rect { x, y, w, h, r, ink } => (
                Some(kurbo::Shape::to_path(
                    &kurbo::RoundedRect::new(
                        f64::from(x),
                        f64::from(y),
                        f64::from(x + w),
                        f64::from(y + h),
                        f64::from(r),
                    ),
                    0.05,
                )),
                ink,
            ),
            Shape::Circle { cx, cy, r, ink } => (
                Some(kurbo::Shape::to_path(
                    &kurbo::Circle::new((f64::from(cx), f64::from(cy)), f64::from(r)),
                    0.05,
                )),
                ink,
            ),
        };
        let Some(path) = path.as_ref().and_then(to_skia) else {
            continue;
        };
        paint(&mut pixmap, &path, ink, color, transform);
    }
    Handle::from_rgba(w, h, straight_rgba(&pixmap))
}

fn paint(
    pixmap: &mut Pixmap,
    path: &tiny_skia::Path,
    ink: Ink,
    color: Color,
    transform: Transform,
) {
    let solid = |c: Color| {
        let mut paint = Paint::default();
        paint.set_color_rgba8(
            (c.r * 255.0) as u8,
            (c.g * 255.0) as u8,
            (c.b * 255.0) as u8,
            (c.a * 255.0) as u8,
        );
        paint.anti_alias = true;
        paint
    };
    let stroke = |width: f32, cap: LineCap| Stroke {
        width,
        line_cap: cap,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    match ink {
        Ink::Fill => pixmap.fill_path(path, &solid(color), FillRule::Winding, transform, None),
        Ink::FillHex(rgb) => pixmap.fill_path(
            path,
            &solid(crate::theme::hex(rgb)),
            FillRule::Winding,
            transform,
            None,
        ),
        Ink::Stroke(w) => pixmap.stroke_path(
            path,
            &solid(color),
            &stroke(w, LineCap::Butt),
            transform,
            None,
        ),
        Ink::StrokeRound(w) => pixmap.stroke_path(
            path,
            &solid(color),
            &stroke(w, LineCap::Round),
            transform,
            None,
        ),
        Ink::StrokeHex(rgb, w) => pixmap.stroke_path(
            path,
            &solid(crate::theme::hex(rgb)),
            &stroke(w, LineCap::Butt),
            transform,
            None,
        ),
    }
}

fn to_skia(path: &kurbo::BezPath) -> Option<tiny_skia::Path> {
    let mut builder = PathBuilder::new();
    let f = |v: f64| v as f32;
    for element in path.elements() {
        match *element {
            kurbo::PathEl::MoveTo(p) => builder.move_to(f(p.x), f(p.y)),
            kurbo::PathEl::LineTo(p) => builder.line_to(f(p.x), f(p.y)),
            kurbo::PathEl::QuadTo(a, b) => builder.quad_to(f(a.x), f(a.y), f(b.x), f(b.y)),
            kurbo::PathEl::CurveTo(a, b, c) => {
                builder.cubic_to(f(a.x), f(a.y), f(b.x), f(b.y), f(c.x), f(c.y))
            }
            kurbo::PathEl::ClosePath => builder.close(),
        }
    }
    builder.finish()
}

/// tiny-skia keeps premultiplied pixels; images take straight RGBA.
pub(crate) fn straight_rgba(pixmap: &Pixmap) -> Vec<u8> {
    pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

use Ink::{Fill, Stroke as Line, StrokeRound};

/// The Glasshouse mark: a window of four panes, one filled.
pub const MARK: Icon = Icon {
    view_box: (104.0, 104.0),
    shapes: &[
        Shape::Rect {
            x: 6.0,
            y: 6.0,
            w: 92.0,
            h: 92.0,
            r: 24.0,
            ink: Line(6.0),
        },
        Shape::Path("M52 14v76M14 52h76", Line(6.0)),
        Shape::Path("M58 58h28v18a16 16 0 01-16 16H58z", Fill),
    ],
};

/// The mark at menu-bar size: heavier lines so it reads at 16 px.
pub const MARK_SMALL: Icon = Icon {
    view_box: (104.0, 104.0),
    shapes: &[
        Shape::Rect {
            x: 6.0,
            y: 6.0,
            w: 92.0,
            h: 92.0,
            r: 24.0,
            ink: Line(10.0),
        },
        Shape::Path("M52 14v76M14 52h76", Line(10.0)),
        Shape::Path("M60 60h26v16a16 16 0 01-16 16H60z", Fill),
    ],
};

/// "Ask or hand off a task" (the command bar).
pub const SPARK: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Path(
        "M8 1.5v3M8 11.5v3M1.5 8h3M11.5 8h3M3.5 3.5l2 2M10.5 10.5l2 2M12.5 3.5l-2 2M5.5 10.5l-2 2",
        Line(1.5),
    )],
};

pub const WIFI: Icon = Icon {
    view_box: (20.0, 20.0),
    shapes: &[
        Shape::Path(
            "M2.5 8a11 11 0 0115 0M5 10.8a7.5 7.5 0 0110 0M7.6 13.5a3.8 3.8 0 014.8 0",
            Line(1.7),
        ),
        Shape::Circle {
            cx: 10.0,
            cy: 16.0,
            r: 0.8,
            ink: Fill,
        },
    ],
};

pub const BATTERY: Icon = Icon {
    view_box: (26.0, 14.0),
    shapes: &[
        Shape::Rect {
            x: 0.75,
            y: 0.75,
            w: 21.5,
            h: 12.5,
            r: 3.0,
            ink: Line(1.3),
        },
        Shape::Rect {
            x: 3.0,
            y: 3.0,
            w: 15.0,
            h: 8.0,
            r: 1.5,
            ink: Fill,
        },
        Shape::Path("M24 5v4", StrokeRound(1.6)),
    ],
};

pub const ACCESSIBILITY: Icon = Icon {
    view_box: (20.0, 20.0),
    shapes: &[
        Shape::Circle {
            cx: 10.0,
            cy: 10.0,
            r: 8.0,
            ink: Line(1.5),
        },
        Shape::Circle {
            cx: 10.0,
            cy: 6.0,
            r: 1.0,
            ink: Fill,
        },
        Shape::Path("M6 8.5h8M10 8.5V12l-2 3.5M10 12l2 3.5", Line(1.5)),
    ],
};

pub const ARROW_RIGHT: Icon = Icon {
    view_box: (14.0, 14.0),
    shapes: &[Shape::Path("M3 7h8M7.5 3.5L11 7l-3.5 3.5", Line(1.8))],
};

/// A step done (first-run setup's side panel).
pub const CHECK: Icon = Icon {
    view_box: (14.0, 14.0),
    shapes: &[Shape::Path("M3 7.5l2.8 2.8L11 4.5", Line(1.8))],
};

/// Expand a tile into the expanded view.
pub const EXPAND: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Path(
        "M9.5 2.5h4v4M6.5 13.5h-4v-4M13.5 2.5L9 7M2.5 13.5L7 9",
        Line(1.5),
    )],
};

/// Shrink the expanded view back to its tile (AW-16).
pub const COLLAPSE: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Path(
        "M9.5 2.5v4h4M6.5 13.5v-4h-4M9.5 6.5L14 2M6.5 9.5L2 14",
        Line(1.5),
    )],
};

pub const PAUSE: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Path("M5.5 3.5v9M10.5 3.5v9", StrokeRound(1.8))],
};

pub const RESUME: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Path("M5 3.2l8 4.8-8 4.8z", Fill)],
};

pub const STOP: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Rect {
        x: 3.5,
        y: 3.5,
        w: 9.0,
        h: 9.0,
        r: 2.0,
        ink: Line(1.6),
    }],
};

/// Take over: the person's own pointer.
pub const POINTER: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Path("M3 2.5l10 4.3-4.3 1.4-1.6 4.3z", Line(1.6))],
};

/// Hand back to the agent.
pub const HAND_BACK: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Path(
        "M6 3.5L2.5 7 6 10.5M3 7h6.5a3.5 3.5 0 010 7H8",
        Line(1.6),
    )],
};

/// Roll back to a snapshot (AW-15).
pub const ROLL_BACK: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[
        Shape::Path("M3.4 6A5 5 0 113 8.5", Line(1.6)),
        Shape::Path("M2.8 2.8v3.4h3.4", Line(1.6)),
    ],
};

/// A snapshot, on the timeline and in the activity log (◇).
pub const SNAPSHOT: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Path("M8 2l6 6-6 6-6-6z", Line(1.6))],
};

/// The snapshot a roll back would return to (◆).
pub const SNAPSHOT_SELECTED: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[Shape::Path("M8 1.5l6.5 6.5-6.5 6.5L1.5 8z", Fill)],
};

/// The agent's cursor on a screen agent's thumbnail.
pub const AGENT_CURSOR: Icon = Icon {
    view_box: (14.0, 16.0),
    shapes: &[
        Shape::Path("M1 1l11 6.5-5 1.2-2.2 4.8z", Fill),
        Shape::Path("M1 1l11 6.5-5 1.2-2.2 4.8z", Ink::StrokeHex(0xFFFFFF, 1.2)),
    ],
};

pub const LAUNCHER: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Rect {
            x: 2.0,
            y: 2.0,
            w: 5.5,
            h: 5.5,
            r: 1.4,
            ink: Fill,
        },
        Shape::Rect {
            x: 10.5,
            y: 2.0,
            w: 5.5,
            h: 5.5,
            r: 1.4,
            ink: Fill,
        },
        Shape::Rect {
            x: 2.0,
            y: 10.5,
            w: 5.5,
            h: 5.5,
            r: 1.4,
            ink: Fill,
        },
        Shape::Rect {
            x: 10.5,
            y: 10.5,
            w: 5.5,
            h: 5.5,
            r: 1.4,
            ink: Fill,
        },
    ],
};

/// Activity Monitor's pulse line.
pub const ACTIVITY: Icon = Icon {
    view_box: (24.0, 24.0),
    shapes: &[Shape::Path(
        "M2 12h4.5l2.5-7 5 14 2.5-7H22",
        Ink::StrokeRound(2.0),
    )],
};

pub const FOLDER: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[Shape::Path(
        "M2 5a1.5 1.5 0 011.5-1.5h3.2l1.6 1.8h6.2A1.5 1.5 0 0116 6.8v6.7a1.5 1.5 0 01-1.5 1.5h-11A1.5 1.5 0 012 13.5z",
        Fill,
    )],
};

pub const MAIL: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Rect {
            x: 2.0,
            y: 4.0,
            w: 14.0,
            h: 10.0,
            r: 1.5,
            ink: Line(1.6),
        },
        Shape::Path("M2.5 5l6.5 5 6.5-5", Line(1.6)),
    ],
};

/// Chat: a speech bubble with two lines of a message.
pub const CHAT: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Path(
            "M4 3.2h10a1.8 1.8 0 011.8 1.8v6.2a1.8 1.8 0 01-1.8 1.8H8.6L5.2 15.6V13H4a1.8 1.8 0 01-1.8-1.8V5A1.8 1.8 0 014 3.2z",
            Line(1.6),
        ),
        Shape::Path("M5.8 6.6h6.4M5.8 9.4h3.8", StrokeRound(1.5)),
    ],
};

/// Artifacts: a page with its corner folded and two lines on it, as the
/// app draws it (ADR 0033).
pub const ARTIFACT: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Path(
            "M5 2.2h5.6l3.6 3.6v9a1.2 1.2 0 01-1.2 1.2H5a1.2 1.2 0 01-1.2-1.2V3.4A1.2 1.2 0 015 2.2z",
            Line(1.6),
        ),
        Shape::Path("M10.4 2.4v3.6H14", Line(1.4)),
        Shape::Path("M6.6 9.4h4.8M6.6 12.2h3", StrokeRound(1.5)),
    ],
};

/// Meet: a video camera.
pub const MEET: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Rect {
            x: 1.8,
            y: 4.6,
            w: 10.0,
            h: 8.8,
            r: 2.0,
            ink: Line(1.6),
        },
        Shape::Path("M11.8 8.2l4.4-2.5v6.6l-4.4-2.5z", Line(1.6)),
    ],
};

/// The badge on one of the person's own versions of an app (TL-5): a
/// pencil, as in "edited, and yours".
pub const REMIX: Icon = Icon {
    view_box: (16.0, 16.0),
    shapes: &[
        Shape::Path("M3 13l.7-3.1 6.8-6.8 2.4 2.4-6.8 6.8z", StrokeRound(1.8)),
        Shape::Path("M8.9 4.7l2.4 2.4", StrokeRound(1.8)),
    ],
};

pub const GLOBE: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Circle {
            cx: 9.0,
            cy: 9.0,
            r: 7.0,
            ink: Line(1.6),
        },
        Shape::Path(
            "M2 9h14M9 2c2.2 2 3.2 4.4 3.2 7S11.2 14 9 16C6.8 14 5.8 11.6 5.8 9S6.8 4 9 2z",
            Line(1.6),
        ),
    ],
};

pub const BAG: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Path("M3.5 6h11l-1 9.5h-9z", Line(1.6)),
        Shape::Path("M6.5 6V4.8a2.5 2.5 0 015 0V6", Line(1.6)),
    ],
};

/// The Trash, empty.
pub const TRASH: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Path("M2.8 4.8h12.4M7 4.6V3h4v1.6", StrokeRound(1.5)),
        Shape::Path("M4.3 5l.9 10.2h7.6L13.7 5", StrokeRound(1.5)),
        Shape::Path("M7.4 7.6v5M10.6 7.6v5", StrokeRound(1.3)),
    ],
};

/// The Trash with something in it: paper over the rim.
pub const TRASH_FULL: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Path("M4.6 4.6l1.2-3.4 2.7 1.8 2.3-2.4 2.6 4z", Fill),
        Shape::Path("M2.8 4.8h12.4", StrokeRound(1.5)),
        Shape::Path("M4.3 5l.9 10.2h7.6L13.7 5", StrokeRound(1.5)),
        Shape::Path("M7.4 7.6v5M10.6 7.6v5", StrokeRound(1.3)),
    ],
};

pub const GEAR: Icon = Icon {
    view_box: (18.0, 18.0),
    shapes: &[
        Shape::Circle {
            cx: 9.0,
            cy: 9.0,
            r: 2.6,
            ink: Line(1.6),
        },
        Shape::Path(
            "M9 1.8v2.4M9 13.8v2.4M1.8 9h2.4M13.8 9h2.4M3.9 3.9l1.7 1.7M12.4 12.4l1.7 1.7M14.1 3.9l-1.7 1.7M5.6 12.4l-1.7 1.7",
            StrokeRound(1.6),
        ),
    ],
};
