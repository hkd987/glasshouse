//! The pointer, painted over the last frame without redrawing the UI:
//! moving it repaints only the rows it leaves and the rows it enters.

use std::ops::Range;

/// An arrow with its tip at the top left: `#` outline, `-` fill.
const ARROW: [&str; 19] = [
    "#",
    "##",
    "#-#",
    "#--#",
    "#---#",
    "#----#",
    "#-----#",
    "#------#",
    "#-------#",
    "#--------#",
    "#---------#",
    "#------#####",
    "#---#--#",
    "#--##--#",
    "#-# #--#",
    "##   #--#",
    "#    #--#",
    "      #--#",
    "      ###",
];

/// The frame rows the arrow covers with its tip on row `y`, on a screen
/// `height` rows tall.
pub fn rows(y: u32, height: u32) -> Range<u32> {
    y.min(height)..(y + ARROW.len() as u32).min(height)
}

/// Paints the arrow with its tip at (`x`, `y`) into `band`: whole rows of
/// 4-byte pixels of a frame `width` pixels wide, starting at frame row
/// `top`. The arrow is gray and white, the same in RGBA or BGRA order.
pub fn paint(band: &mut [u8], width: u32, top: u32, x: u32, y: u32) {
    let stride = width as usize * 4;
    for (dy, line) in ARROW.iter().enumerate() {
        let Some(row) = (y as usize + dy).checked_sub(top as usize) else {
            continue;
        };
        for (dx, c) in line.bytes().enumerate() {
            let rgb = match c {
                b'#' => [0x1a, 0x1a, 0x1a],
                b'-' => [0xff, 0xff, 0xff],
                _ => continue,
            };
            let px = x as usize + dx;
            if px >= width as usize {
                continue;
            }
            let i = row * stride + px * 4;
            if let Some(pixel) = band.get_mut(i..i + 4) {
                pixel.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xff]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arrow_is_painted_from_its_tip_and_clipped_to_the_band() {
        let (width, top) = (20u32, 5u32);
        let mut band = vec![0u8; width as usize * 4 * 10];
        paint(&mut band, width, top, 3, 6);
        let at = |x: usize, row: usize| &band[(row * width as usize + x) * 4..][..4];
        // The tip is frame row 6, band row 1; nothing above it.
        assert_eq!(at(3, 0), [0, 0, 0, 0]);
        assert_eq!(at(3, 1), [0x1a, 0x1a, 0x1a, 0xff]);
        assert_eq!(at(4, 3), [0xff, 0xff, 0xff, 0xff]);
        assert_eq!(rows(795, 800), 795..800);
    }
}
