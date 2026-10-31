//! Dirty-rectangle tracking: the smallest set of row bands (each with the exact x-extent of its changed
//! pixels) that covers every pixel differing between the image on screen and the next one.
use ink_kindle_hal::{GrayImage, Rect};

/// Rows closer than this are merged into one band (one EPDC update instead of many thin ones).
pub const MERGE_GAP_ROWS: u32 = 24;

/// Changed regions between `prev` and `next` (same size). At most `max_rects` rectangles: the closest
/// bands are merged when there are more. Empty when the images are identical.
pub fn diff(prev: &GrayImage, next: &GrayImage, max_rects: usize) -> Vec<Rect> {
    assert_eq!(prev.size, next.size, "diff needs equal sizes");
    let w = prev.size.w as usize;
    let mut bands: Vec<Rect> = Vec::new();
    for y in 0..prev.size.h {
        let a = prev.row(y);
        let b = next.row(y);
        if a == b {
            continue;
        }
        let x0 = a.iter().zip(b).position(|(p, q)| p != q).unwrap_or(0);
        let x1 = w - a.iter().zip(b).rev().position(|(p, q)| p != q).unwrap_or(0);
        let row = Rect::new(x0 as u32, y, (x1 - x0) as u32, 1);
        match bands.last_mut() {
            Some(last) if y - last.bottom() < MERGE_GAP_ROWS => *last = last.union(&row),
            _ => bands.push(row),
        }
    }
    let max_rects = max_rects.max(1);
    while bands.len() > max_rects {
        // merge the adjacent pair with the smallest vertical gap
        let i = (0..bands.len() - 1)
            .min_by_key(|&i| bands[i + 1].y - bands[i].bottom())
            .expect("at least two bands");
        let merged = bands[i].union(&bands[i + 1]);
        bands[i] = merged;
        bands.remove(i + 1);
    }
    bands
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_kindle_hal::Size;

    #[test]
    fn identical_images_have_no_dirty_region() {
        let a = GrayImage::new(Size { w: 50, h: 40 }, 0xff);
        assert!(diff(&a, &a.clone(), 4).is_empty());
    }

    #[test]
    fn single_pixel_and_exact_extents() {
        let a = GrayImage::new(Size { w: 50, h: 40 }, 0xff);
        let mut b = a.clone();
        b.set(17, 9, 0);
        assert_eq!(diff(&a, &b, 4), vec![Rect::new(17, 9, 1, 1)]);
        b.set(30, 12, 0);
        assert_eq!(diff(&a, &b, 4), vec![Rect::new(17, 9, 14, 4)]);
    }

    #[test]
    fn far_apart_changes_stay_separate_until_the_cap() {
        let a = GrayImage::new(Size { w: 100, h: 300 }, 0xff);
        let mut b = a.clone();
        b.set(1, 0, 0);
        b.set(2, 100, 0);
        b.set(3, 140, 0);
        b.set(4, 299, 0);
        assert_eq!(diff(&a, &b, 8).len(), 4);
        let two = diff(&a, &b, 2);
        assert_eq!(two.len(), 2);
        // the 100/140 pair (gap 39) merges first, then 0 with them (gap 99) rather than 299 (gap 158)
        assert_eq!(two, vec![Rect::new(1, 0, 3, 141), Rect::new(4, 299, 1, 1)]);
    }
}
