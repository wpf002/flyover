//! Squarified treemap (Bruls, Huizing, van Wijk 2000).
//!
//! Given a rectangle and a list of item areas that sum to the rectangle's area, it partitions the
//! rectangle into sub-rectangles of those exact areas, greedily keeping aspect ratios near 1. The
//! partition is complete (children tile the parent with no gaps), which is what makes "sum of cell
//! areas equals world area" hold. Deterministic: it only depends on the input order and areas.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn area(&self) -> f64 {
        self.w * self.h
    }
    pub fn centroid(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// Partition `rect` into one sub-rect per area, in the same order as `areas`.
/// `areas` should sum to `rect.area()`; small drift is absorbed by the last row.
pub fn squarify(areas: &[f64], rect: Rect) -> Vec<Rect> {
    let mut out = vec![
        Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0
        };
        areas.len()
    ];
    if areas.is_empty() {
        return out;
    }

    let mut remaining = rect;
    let mut i = 0;
    while i < areas.len() {
        let side = remaining.w.min(remaining.h);
        // Grow the current row while it improves (lowers) the worst aspect ratio.
        let mut row_end = i + 1;
        let mut row_sum = areas[i];
        let mut row_min = areas[i];
        let mut row_max = areas[i];
        while row_end < areas.len() {
            let a = areas[row_end];
            let next_sum = row_sum + a;
            let next_min = row_min.min(a);
            let next_max = row_max.max(a);
            if worst(next_sum, next_min, next_max, side) <= worst(row_sum, row_min, row_max, side) {
                row_sum = next_sum;
                row_min = next_min;
                row_max = next_max;
                row_end += 1;
            } else {
                break;
            }
        }
        remaining = lay_row(&areas[i..row_end], row_sum, remaining, &mut out[i..row_end]);
        i = row_end;
    }
    out
}

/// Worst (largest) aspect ratio in a row of total area `sum`, spanning length `side`.
fn worst(sum: f64, min: f64, max: f64, side: f64) -> f64 {
    if sum <= 0.0 || side <= 0.0 {
        return f64::INFINITY;
    }
    let side2 = side * side;
    let sum2 = sum * sum;
    ((side2 * max) / sum2).max(sum2 / (side2 * min))
}

/// Place a row of items along the shorter side of `remaining`, writing rects into `dst`, and
/// return the rectangle left over for the next rows.
fn lay_row(areas: &[f64], sum: f64, remaining: Rect, dst: &mut [Rect]) -> Rect {
    if remaining.w <= remaining.h {
        // Horizontal band across the full width, stacked downward.
        let band_h = if remaining.w > 0.0 {
            sum / remaining.w
        } else {
            0.0
        };
        let mut x = remaining.x;
        for (k, &a) in areas.iter().enumerate() {
            let w = if band_h > 0.0 { a / band_h } else { 0.0 };
            dst[k] = Rect {
                x,
                y: remaining.y,
                w,
                h: band_h,
            };
            x += w;
        }
        Rect {
            x: remaining.x,
            y: remaining.y + band_h,
            w: remaining.w,
            h: (remaining.h - band_h).max(0.0),
        }
    } else {
        // Vertical band down the full height, filled rightward.
        let band_w = if remaining.h > 0.0 {
            sum / remaining.h
        } else {
            0.0
        };
        let mut y = remaining.y;
        for (k, &a) in areas.iter().enumerate() {
            let h = if band_w > 0.0 { a / band_w } else { 0.0 };
            dst[k] = Rect {
                x: remaining.x,
                y,
                w: band_w,
                h,
            };
            y += h;
        }
        Rect {
            x: remaining.x + band_w,
            y: remaining.y,
            w: (remaining.w - band_w).max(0.0),
            h: remaining.h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(area: f64) -> Rect {
        let s = area.sqrt();
        Rect {
            x: 0.0,
            y: 0.0,
            w: s,
            h: s,
        }
    }

    #[test]
    fn areas_are_conserved() {
        let areas = vec![6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let total: f64 = areas.iter().sum();
        let rects = squarify(&areas, root(total));
        let sum: f64 = rects.iter().map(Rect::area).sum();
        assert!((sum - total).abs() / total < 1e-9, "sum {sum} vs {total}");
        for (want, got) in areas.iter().zip(&rects) {
            assert!(
                (want - got.area()).abs() / want < 1e-6,
                "area {want} vs {}",
                got.area()
            );
        }
    }

    #[test]
    fn cells_stay_within_bounds() {
        let areas = vec![10.0, 5.0, 3.0, 1.0, 1.0];
        let total: f64 = areas.iter().sum();
        let r = root(total);
        for cell in squarify(&areas, r) {
            assert!(cell.x >= -1e-9 && cell.y >= -1e-9);
            assert!(cell.x + cell.w <= r.w + 1e-6);
            assert!(cell.y + cell.h <= r.h + 1e-6);
        }
    }

    #[test]
    fn deterministic() {
        let areas = vec![3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0];
        let r = root(areas.iter().sum());
        assert_eq!(squarify(&areas, r), squarify(&areas, r));
    }

    #[test]
    fn single_item_fills_rect() {
        let r = Rect {
            x: 2.0,
            y: 3.0,
            w: 4.0,
            h: 5.0,
        };
        let got = squarify(&[20.0], r);
        assert_eq!(got[0], r);
    }
}
