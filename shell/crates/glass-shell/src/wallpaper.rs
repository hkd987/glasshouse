//! The wallpaper, generated from the theme rather than shipped as an image:
//! soft shapes on a base color, blurred. It's drawn at a quarter of the
//! screen size, where the blur is cheap and loses nothing, and the renderer
//! scales it up.

use std::sync::Mutex;

use iced_core::image::Handle;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

use crate::theme::Wallpaper;

/// The design size the wallpaper's shapes are laid out on.
const DESIGN: (f32, f32) = (1440.0, 900.0);
const DOWNSCALE: f32 = 4.0;

/// The wallpaper for a screen of `width` x `height` logical pixels, cached
/// for the last theme and size asked for.
pub fn handle(wallpaper: &Wallpaper, width: f32, height: f32) -> Handle {
    type Cached = Option<(Wallpaper, (u32, u32), Handle)>;
    static CACHE: Mutex<Cached> = Mutex::new(None);
    let size = (
        (width / DOWNSCALE).ceil() as u32,
        (height / DOWNSCALE).ceil() as u32,
    );
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((theme, cached_size, handle)) = cache.as_ref()
        && theme == wallpaper
        && *cached_size == size
    {
        return handle.clone();
    }
    let rgba = render(wallpaper, size.0, size.1);
    let handle = Handle::from_rgba(size.0, size.1, rgba);
    *cache = Some((wallpaper.clone(), size, handle.clone()));
    handle
}

/// Straight RGBA pixels, opaque.
pub fn render(wallpaper: &Wallpaper, width: u32, height: u32) -> Vec<u8> {
    let Some(mut shapes) = Pixmap::new(width.max(1), height.max(1)) else {
        return vec![0; (width * height * 4) as usize];
    };
    let scale = (width as f32 / DESIGN.0).min(height as f32 / DESIGN.1);
    for blob in &wallpaper.blobs {
        let mut builder = PathBuilder::new();
        let Some(rect) = Rect::from_xywh(blob.x, blob.y, blob.width, blob.height) else {
            continue;
        };
        push_rounded_rect(&mut builder, rect, blob.radius);
        let Some(path) = builder.finish() else {
            continue;
        };
        let (cx, cy) = (blob.x + blob.width / 2.0, blob.y + blob.height / 2.0);
        let transform = Transform::from_scale(scale, scale).pre_concat(Transform::from_rotate_at(
            blob.degrees,
            cx,
            cy,
        ));
        let mut paint = Paint::default();
        let c = blob.color;
        paint.set_color_rgba8(
            (c.r * 255.0) as u8,
            (c.g * 255.0) as u8,
            (c.b * 255.0) as u8,
            (c.a * 255.0) as u8,
        );
        paint.anti_alias = true;
        shapes.fill_path(&path, &paint, FillRule::Winding, transform, None);
    }

    // Blur the shapes layer on its own (beyond the edges is transparent, as
    // with a CSS blur), then lay it over the base color.
    let mut layer: Vec<[f32; 4]> = shapes
        .pixels()
        .iter()
        .map(|p| {
            [
                f32::from(p.red()),
                f32::from(p.green()),
                f32::from(p.blue()),
                f32::from(p.alpha()),
            ]
        })
        .collect();
    gaussian_blur(
        &mut layer,
        width as usize,
        height as usize,
        wallpaper.blur * scale,
    );
    let base = wallpaper.base;
    let base = [base.r * 255.0, base.g * 255.0, base.b * 255.0];
    layer
        .iter()
        .flat_map(|[r, g, b, a]| {
            let keep = 1.0 - a / 255.0;
            [
                (r + base[0] * keep).round().clamp(0.0, 255.0) as u8,
                (g + base[1] * keep).round().clamp(0.0, 255.0) as u8,
                (b + base[2] * keep).round().clamp(0.0, 255.0) as u8,
                255,
            ]
        })
        .collect()
}

fn push_rounded_rect(builder: &mut PathBuilder, rect: Rect, radius: f32) {
    let (l, t, r, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let k = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    // Circular corners as cubics (0.5523 is the usual quarter-circle factor).
    let c = k * (1.0 - 0.552_284_8);
    builder.move_to(l + k, t);
    builder.line_to(r - k, t);
    builder.cubic_to(r - c, t, r, t + c, r, t + k);
    builder.line_to(r, b - k);
    builder.cubic_to(r, b - c, r - c, b, r - k, b);
    builder.line_to(l + k, b);
    builder.cubic_to(l + c, b, l, b - c, l, b - k);
    builder.line_to(l, t + k);
    builder.cubic_to(l, t + c, l + c, t, l + k, t);
    builder.close();
}

/// A Gaussian blur of `sigma` pixels, as three box blurs (premultiplied
/// pixels, transparent beyond the edges).
fn gaussian_blur(pixels: &mut [[f32; 4]], width: usize, height: usize, sigma: f32) {
    if sigma < 0.5 || width == 0 || height == 0 {
        return;
    }
    let mut scratch = vec![[0.0; 4]; pixels.len()];
    for size in box_sizes(sigma, 3) {
        let radius = (size - 1) / 2;
        box_blur(pixels, &mut scratch, width, height, radius, true);
        box_blur(&mut scratch, pixels, width, height, radius, false);
    }
}

/// Box widths whose repeated application approximates a Gaussian.
fn box_sizes(sigma: f32, n: usize) -> Vec<usize> {
    let ideal = ((12.0 * sigma * sigma / n as f32) + 1.0).sqrt();
    let mut lower = ideal.floor() as usize;
    if lower.is_multiple_of(2) {
        lower -= 1;
    }
    let upper = lower + 2;
    let m = ((12.0 * sigma * sigma
        - (n * lower * lower) as f32
        - (4 * n * lower) as f32
        - (3 * n) as f32)
        / (-4.0 * lower as f32 - 4.0))
        .round() as usize;
    (0..n).map(|i| if i < m { lower } else { upper }).collect()
}

/// One pass of a running-sum box blur, from `src` into `dst`, along rows
/// (`horizontal`) or columns.
fn box_blur(
    src: &mut [[f32; 4]],
    dst: &mut [[f32; 4]],
    width: usize,
    height: usize,
    radius: usize,
    horizontal: bool,
) {
    let (lines, len) = if horizontal {
        (height, width)
    } else {
        (width, height)
    };
    let index = |line: usize, i: usize| {
        if horizontal {
            line * width + i
        } else {
            i * width + line
        }
    };
    let norm = 1.0 / (2 * radius + 1) as f32;
    for line in 0..lines {
        let mut sum = [0.0f32; 4];
        for i in 0..=radius.min(len - 1) {
            let p = src[index(line, i)];
            for c in 0..4 {
                sum[c] += p[c];
            }
        }
        for i in 0..len {
            let out = &mut dst[index(line, i)];
            for c in 0..4 {
                out[c] = sum[c] * norm;
            }
            if i + radius + 1 < len {
                let p = src[index(line, i + radius + 1)];
                for c in 0..4 {
                    sum[c] += p[c];
                }
            }
            if i >= radius {
                let p = src[index(line, i - radius)];
                for c in 0..4 {
                    sum[c] -= p[c];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn the_wallpaper_is_opaque_and_varies() {
        let pixels = render(&Theme::glass().wallpaper, 360, 225);
        assert_eq!(pixels.len(), 360 * 225 * 4);
        assert!(pixels.chunks(4).all(|p| p[3] == 255));
        let first = &pixels[..4];
        assert!(pixels.chunks(4).any(|p| p != first), "the shapes show");
    }

    #[test]
    fn box_sizes_cover_the_sigma() {
        let sizes = box_sizes(12.0, 3);
        assert_eq!(sizes.len(), 3);
        assert!(sizes.iter().all(|s| s % 2 == 1));
    }
}
