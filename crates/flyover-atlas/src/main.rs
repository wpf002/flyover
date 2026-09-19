//! Bakes the renderer's font atlas: one signed-distance-field cell per printable ASCII character
//! of a monospace font, written to `crates/flyover-render/assets/font-atlas.bin`.
//!
//! Run it only when the font or the layout constants change:
//! `cargo run -p flyover-atlas` (the output is committed).
//!
//! Baking offline keeps the font parser and the distance-field code out of both shipped binaries;
//! the renderer and the wasm build only carry the finished atlas.
//!
//! Each cell covers the character's advance box grown by a padding margin, so a glyph quad may
//! overlap its neighbours — which is what an SDF wants. The field is built by rasterizing at
//! `SUPERSAMPLE` times the final size, running a Euclidean distance transform over the coverage,
//! and downsampling the distances.

use std::path::PathBuf;

use anyhow::{Context, Result};
use fontdue::{Font, FontSettings};

const FONT: &[u8] = include_bytes!("../../flyover-render/assets/JetBrainsMono-Regular.ttf");

/// Printable ASCII. Anything else renders as a blank cell.
const FIRST_CHAR: u32 = 32;
const LAST_CHAR: u32 = 126;
/// Final height of one cell, in atlas pixels.
const CELL_H: usize = 64;
/// Padding around the advance box, as a fraction of the line height.
const PAD: f32 = 0.12;
/// Rasterize this many times larger, then downsample the distances.
const SUPERSAMPLE: usize = 4;
/// Distances are stored clamped to this many final-resolution pixels either side of the edge.
const RANGE_PX: f32 = 6.0;
const MAGIC: &[u8; 4] = b"FATL";
const VERSION: u32 = 1;

fn main() -> Result<()> {
    let font = Font::from_bytes(FONT, FontSettings::default())
        .map_err(|e| anyhow::anyhow!("parsing the font: {e}"))?;

    // Monospace: every glyph shares an advance. Measure it on 'M' at one em.
    let em = 1000.0f32;
    let advance_em = font.metrics('M', em).advance_width / em;
    let line = font
        .horizontal_line_metrics(em)
        .context("font has no horizontal metrics")?;
    let ascent_em = line.ascent / em;
    let descent_em = line.descent / em; // negative
    let line_h_em = (line.ascent - line.descent + line.line_gap) / em;

    let pad_em = PAD * line_h_em;
    let quad_w_em = advance_em + 2.0 * pad_em;
    let quad_h_em = (ascent_em - descent_em) + 2.0 * pad_em;

    let px_per_em = CELL_H as f32 / quad_h_em;
    let cell_w = (quad_w_em * px_per_em).round().max(1.0) as usize;
    let count = (LAST_CHAR - FIRST_CHAR + 1) as usize;
    let cols = 16usize;
    let rows = count.div_ceil(cols);
    let (atlas_w, atlas_h) = (cols * cell_w, rows * CELL_H);
    let mut pixels = vec![128u8; atlas_w * atlas_h]; // 128 = on the edge, i.e. empty

    for index in 0..count {
        let ch = char::from_u32(FIRST_CHAR + index as u32).unwrap();
        let cell = bake_cell(&font, ch, cell_w, px_per_em, pad_em, ascent_em);
        let (cx, cy) = (index % cols, index / cols);
        for y in 0..CELL_H {
            let dst = (cy * CELL_H + y) * atlas_w + cx * cell_w;
            pixels[dst..dst + cell_w].copy_from_slice(&cell[y * cell_w..(y + 1) * cell_w]);
        }
    }

    let out =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../flyover-render/assets/font-atlas.bin");
    let mut bytes = Vec::with_capacity(64 + pixels.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    for v in [
        atlas_w as u32,
        atlas_h as u32,
        cell_w as u32,
        CELL_H as u32,
        cols as u32,
        count as u32,
        FIRST_CHAR,
    ] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    // Geometry of one character cell, in em units: quad size, the quad's lower-left corner
    // relative to the pen origin on the baseline, the advance, and the line height.
    for v in [
        quad_w_em,
        quad_h_em,
        -pad_em,
        descent_em - pad_em,
        advance_em,
        line_h_em,
        RANGE_PX / CELL_H as f32 * quad_h_em,
    ] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.extend_from_slice(&pixels);
    std::fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;

    println!(
        "wrote {} ({} bytes): {}x{} atlas, {} cells of {}x{}, advance {:.3} em, line {:.3} em",
        out.display(),
        bytes.len(),
        atlas_w,
        atlas_h,
        count,
        cell_w,
        CELL_H,
        advance_em,
        line_h_em
    );
    Ok(())
}

/// One character's distance field, `cell_w * CELL_H` bytes.
fn bake_cell(
    font: &Font,
    ch: char,
    cell_w: usize,
    px_per_em: f32,
    pad_em: f32,
    ascent_em: f32,
) -> Vec<u8> {
    let (sw, sh) = (cell_w * SUPERSAMPLE, CELL_H * SUPERSAMPLE);
    let mut inside = vec![false; sw * sh];

    let size = px_per_em * SUPERSAMPLE as f32;
    let (metrics, coverage) = font.rasterize(ch, size);
    if metrics.width > 0 && metrics.height > 0 {
        // Pen origin inside the supersampled cell: padding to the left, ascent + padding above.
        let origin_x = pad_em * size;
        let origin_y = (ascent_em + pad_em) * size;
        let left = origin_x + metrics.xmin as f32;
        let top = origin_y - (metrics.ymin + metrics.height as i32) as f32;
        for gy in 0..metrics.height {
            for gx in 0..metrics.width {
                if coverage[gy * metrics.width + gx] < 128 {
                    continue;
                }
                let x = (left + gx as f32).round();
                let y = (top + gy as f32).round();
                if x < 0.0 || y < 0.0 {
                    continue;
                }
                let (x, y) = (x as usize, y as usize);
                if x < sw && y < sh {
                    inside[y * sw + x] = true;
                }
            }
        }
    }

    let distance = signed_distance(&inside, sw, sh);
    // Downsample: average the supersampled distances covering each final pixel, convert to px at
    // the final resolution, then encode with 128 on the edge.
    let mut cell = vec![0u8; cell_w * CELL_H];
    let area = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    for y in 0..CELL_H {
        for x in 0..cell_w {
            let mut sum = 0.0;
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    sum += distance[(y * SUPERSAMPLE + sy) * sw + x * SUPERSAMPLE + sx];
                }
            }
            let d = sum / area / SUPERSAMPLE as f32; // supersampled px -> final px
            let t = (d / RANGE_PX).clamp(-1.0, 1.0);
            cell[y * cell_w + x] = ((t * 0.5 + 0.5) * 255.0).round() as u8;
        }
    }
    cell
}

/// Signed distance to the nearest edge, positive inside. Two passes of 8-point sequential
/// Euclidean distance mapping over each side, which is exact enough at this resolution.
fn signed_distance(inside: &[bool], w: usize, h: usize) -> Vec<f32> {
    let outside_d = edt(inside, w, h, true);
    let inside_d = edt(inside, w, h, false);
    (0..w * h)
        .map(|i| {
            if inside[i] {
                inside_d[i]
            } else {
                -outside_d[i]
            }
        })
        .collect()
}

/// Distance from every pixel to the nearest pixel whose `inside` differs from `seed_inside`.
fn edt(inside: &[bool], w: usize, h: usize, seed_inside: bool) -> Vec<f32> {
    const FAR: f32 = 1e9;
    // Nearest-seed offset per pixel.
    let mut dx = vec![0i32; w * h];
    let mut dy = vec![0i32; w * h];
    let mut dist = vec![FAR; w * h];
    for i in 0..w * h {
        if inside[i] == seed_inside {
            dist[i] = 0.0;
        }
    }

    let relax =
        |i: usize, j: usize, ox: i32, oy: i32, dx: &mut [i32], dy: &mut [i32], dist: &mut [f32]| {
            let (nx, ny) = (dx[j] + ox, dy[j] + oy);
            let nd = ((nx * nx + ny * ny) as f32).sqrt();
            if nd < dist[i] {
                dist[i] = nd;
                dx[i] = nx;
                dy[i] = ny;
            }
        };

    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if x > 0 {
                relax(i, i - 1, 1, 0, &mut dx, &mut dy, &mut dist);
            }
            if y > 0 {
                relax(i, i - w, 0, 1, &mut dx, &mut dy, &mut dist);
                if x > 0 {
                    relax(i, i - w - 1, 1, 1, &mut dx, &mut dy, &mut dist);
                }
                if x + 1 < w {
                    relax(i, i - w + 1, -1, 1, &mut dx, &mut dy, &mut dist);
                }
            }
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if x + 1 < w {
                relax(i, i + 1, -1, 0, &mut dx, &mut dy, &mut dist);
            }
            if y + 1 < h {
                relax(i, i + w, 0, -1, &mut dx, &mut dy, &mut dist);
                if x + 1 < w {
                    relax(i, i + w + 1, -1, -1, &mut dx, &mut dy, &mut dist);
                }
                if x > 0 {
                    relax(i, i + w - 1, 1, -1, &mut dx, &mut dy, &mut dist);
                }
            }
        }
    }
    dist
}
