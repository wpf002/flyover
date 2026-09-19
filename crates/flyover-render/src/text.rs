//! Source text on the roofs (docs/SPEC.md 2.4, M4).
//!
//! Three levels of detail, picked per file per frame from the projected height of one text line:
//!
//! | line height on screen | what is drawn |
//! |---|---|
//! | >= [`GLYPH_MIN_PX`] | one SDF glyph quad per character, colored by token class |
//! | [`STRIP_MIN_PX`]..[`GLYPH_MIN_PX`] | one colored bar per token run, no glyphs |
//! | < [`STRIP_MIN_PX`] | nothing: the roof's own color |
//!
//! Both tiers come out of one layout pass ([`layout`]) and live in one instance buffer, so
//! switching tier is a change of draw range, not a rebuild. Layout runs on a worker thread (native
//! pool or browser web worker), never the render thread.
//!
//! The grid is monospace: a file's lines are fitted into its roof rectangle at whatever em size
//! makes the longest line and the line count both fit, so a file's shape on screen is its shape in
//! an editor. The whole file sets that size, but only a [`WINDOW`] of lines around where the
//! camera is looking is turned into quads: a 12,000-line file is half a million characters, and
//! at any readable zoom fewer than 200 of its lines are on screen. The window is aligned to a
//! coarse grid ([`Placement::window_for`]) so panning down a file re-lays it out in steps rather
//! than every frame.

use bytemuck::{Pod, Zeroable};

use crate::font::Metrics;
use flyover_tiles::text::{TextTile, TokenSpan};

/// Line height in pixels at or above which characters are drawn as glyphs.
pub const GLYPH_MIN_PX: f32 = 6.0;
/// Line height in pixels below which a roof draws no text at all.
pub const STRIP_MIN_PX: f32 = 1.0;
/// A roof smaller than this on screen never asks for its text.
pub const ROOF_MIN_PX: f32 = 24.0;
/// `cell` value marking an instance as a flat colored quad rather than a glyph.
pub const SOLID: u32 = u32::MAX;

/// Lines turned into quads in one layout. The window covers far more than any readable zoom can
/// show, so scrolling within it never reveals an edge.
pub const WINDOW: u32 = 1024;
/// The window starts on a multiple of this, so small camera moves do not re-lay out the file.
const WINDOW_STEP: u32 = WINDOW / 2;
/// Columns past this are not laid out; long minified lines would otherwise shrink the whole grid.
const MAX_COLS: usize = 120;
/// A tab advances to the next multiple of this many columns.
const TAB: usize = 4;
/// Column count assumed by [`estimated_line_height`] before a file's text has been read.
const TYPICAL_COLS: usize = 80;
/// Per-file instance caps, so one pathological file cannot dominate the text budget. A window of
/// [`WINDOW`] ordinary lines stays well inside them.
const MAX_GLYPHS: usize = 48_000;
const MAX_STRIPS: usize = 12_000;
/// Fraction of the rectangle the text block may not use, split between the two sides. A file's
/// text keeps the aspect its lines give it, so the block rarely matches the roof: it is centred
/// in the leftover space rather than pinned to a corner.
const MARGIN: f32 = 0.06;
/// Height of a token bar, as a fraction of the line height (roughly cap height).
const STRIP_HEIGHT: f32 = 0.62;

/// One textured or solid quad lying flat on a roof. `rect` is world-space `[x, y, w, h]` with the
/// origin at the lower-left corner; `cell` indexes the font atlas, or is [`SOLID`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct GlyphInstance {
    pub rect: [f32; 4],
    pub z: f32,
    pub cell: u32,
    /// Packed 0xAABBGGRR.
    pub color: u32,
    pub _pad: u32,
}

/// Where a file's text goes: its roof rectangle, the height of that roof, and which slice of the
/// file to build quads for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub file_id: u32,
    /// World-space `[x, y, w, h]`, lower-left origin.
    pub rect: [f32; 4],
    /// Roof height. The text is lifted a hair above it by the renderer's depth offset.
    pub z: f32,
    /// First line of the [`WINDOW`] this layout covers. Always a multiple of `WINDOW / 2`.
    pub first_line: u32,
}

impl Placement {
    /// The window holding `line`, snapped so the line sits in its middle half. Panning inside a
    /// window costs nothing; leaving it re-lays the file out once.
    pub fn window_for(line: u32) -> u32 {
        ((line / WINDOW_STEP) * WINDOW_STEP).saturating_sub(WINDOW_STEP / 2)
    }
}

/// A request for one file's text, carrying the placement so a worker can lay it out fully.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextRequest {
    pub placement: Placement,
    pub priority: i64,
}

/// A laid-out file, ready to upload. The instances are ordered so each tier is one contiguous
/// draw: a backing quad then the glyphs, then a second backing quad and the token bars.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedText {
    pub file_id: u32,
    pub instances: Vec<GlyphInstance>,
    /// Number of glyphs after the first backing quad.
    pub glyph_count: u32,
    /// Number of bars after the second backing quad.
    pub strip_count: u32,
    /// First line this layout covers; it holds [`WINDOW`] lines from there.
    pub first_line: u32,
    /// Height of one line in world units, so the render thread can pick the tier exactly rather
    /// than from the estimate it used before the text arrived.
    pub line_world: f32,
}

impl PreparedText {
    /// Instance range to draw for the glyph tier.
    pub fn glyph_range(&self) -> std::ops::Range<u32> {
        0..1 + self.glyph_count
    }
    /// Instance range to draw for the bar tier.
    pub fn strip_range(&self) -> std::ops::Range<u32> {
        let start = 1 + self.glyph_count;
        start..start + 1 + self.strip_count
    }
    pub fn bytes(&self) -> u64 {
        (self.instances.len() * std::mem::size_of::<GlyphInstance>()) as u64
    }
}

/// Colors for the ten token classes of `flyover_index::tokens::TokenClass`, plus the dark backing
/// quad the text sits on so it reads against any roof color.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub classes: [[f32; 3]; 10],
    pub backing: [f32; 4],
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            classes: [
                [0.78, 0.80, 0.86], // Other
                [0.83, 0.55, 0.92], // Keyword
                [0.60, 0.85, 0.55], // String
                [0.44, 0.49, 0.58], // Comment
                [0.96, 0.72, 0.42], // Number
                [0.42, 0.80, 0.90], // Type
                [0.44, 0.68, 0.98], // Function
                [0.85, 0.87, 0.92], // Variable
                [0.95, 0.52, 0.55], // Operator
                [0.58, 0.62, 0.70], // Punctuation
            ],
            backing: [0.055, 0.065, 0.085, 0.88],
        }
    }
}

impl Palette {
    fn class(&self, class: u8) -> u32 {
        let c = self.classes[(class as usize).min(self.classes.len() - 1)];
        pack(c[0], c[1], c[2], 1.0)
    }
}

fn pack(r: f32, g: f32, b: f32, a: f32) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    q(r) | (q(g) << 8) | (q(b) << 16) | (q(a) << 24)
}

/// Which tier a roof is at, given the height of one of its text lines in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    None,
    Strips,
    Glyphs,
}

pub fn tier(line_px: f32) -> Tier {
    if line_px >= GLYPH_MIN_PX {
        Tier::Glyphs
    } else if line_px >= STRIP_MIN_PX {
        Tier::Strips
    } else {
        Tier::None
    }
}

/// Height of one text line in world units for a file laid out in `rect`.
pub fn line_world_height(metrics: &Metrics, rect: [f32; 4], grid: (usize, usize)) -> f32 {
    metrics.line_height * em_scale(metrics, rect, grid)
}

/// Line height guessed from a file's line count alone, for deciding whether to fetch its `.ftx`
/// before the real column width is known. Replaced by [`PreparedText::line_world`] once it lands.
pub fn estimated_line_height(metrics: &Metrics, rect: [f32; 4], lines: u32) -> f32 {
    line_world_height(metrics, rect, (TYPICAL_COLS, lines.max(1) as usize))
}

/// The em size that fits a `cols x lines` grid inside the rectangle.
fn em_scale(metrics: &Metrics, rect: [f32; 4], (cols, lines): (usize, usize)) -> f32 {
    let usable_w = rect[2] * (1.0 - 2.0 * MARGIN);
    let usable_h = rect[3] * (1.0 - 2.0 * MARGIN);
    let by_w = usable_w / (cols.max(1) as f32 * metrics.advance);
    let by_h = usable_h / (lines.max(1) as f32 * metrics.line_height);
    by_w.min(by_h).max(0.0)
}

/// Longest line (in expanded columns) and the file's line count. The whole file is measured, not
/// just the window being drawn: the em size has to be the same whichever slice is laid out, or
/// the text would change size as the camera moves down a file.
pub fn grid_of(text: &str) -> (usize, usize) {
    let mut cols = 1usize;
    let mut lines = 0usize;
    for line in text.lines() {
        lines += 1;
        let mut col = 0usize;
        for ch in line.chars() {
            col = if ch == '\t' {
                (col / TAB + 1) * TAB
            } else {
                col + 1
            };
            if col >= MAX_COLS {
                break;
            }
        }
        cols = cols.max(col.min(MAX_COLS));
    }
    (cols, lines.max(1))
}

/// Lay one file's source out on its roof, producing both tiers in a single pass over the text.
///
/// Pure and deterministic: the same tile and placement always give the same instances.
pub fn layout(
    metrics: &Metrics,
    tile: &TextTile,
    placement: Placement,
    palette: &Palette,
) -> PreparedText {
    let rect = placement.rect;
    let grid = grid_of(&tile.text);
    let em = em_scale(metrics, rect, grid);

    let backing = GlyphInstance {
        rect,
        z: placement.z,
        cell: SOLID,
        color: pack(
            palette.backing[0],
            palette.backing[1],
            palette.backing[2],
            palette.backing[3],
        ),
        _pad: 0,
    };
    let mut glyphs = Vec::new();
    let mut strips = Vec::new();
    if em <= 0.0 || tile.text.is_empty() {
        return PreparedText {
            file_id: placement.file_id,
            instances: vec![backing, backing],
            glyph_count: 0,
            strip_count: 0,
            first_line: placement.first_line,
            line_world: 0.0,
        };
    }

    let atlas = crate::font::Atlas::bundled();
    // Centre the block: fitting a long file to a wide roof leaves most of the roof empty on one
    // axis, and a ribbon of code pinned to a corner reads as a bug.
    let block_w = grid.0 as f32 * metrics.advance * em;
    let block_h = grid.1 as f32 * metrics.line_height * em;
    let x_left = rect[0] + (rect[2] - block_w) * 0.5;
    let y_top = rect[1] + (rect[3] + block_h) * 0.5;
    // The atlas pads each cell by `-origin_x` on every side, so the ascent falls out of the quad.
    let ascent = metrics.origin_y + metrics.quad_h + metrics.origin_x;
    let advance = metrics.advance * em;
    let baseline_of = |line: usize| y_top - (line as f32 * metrics.line_height + ascent) * em;

    // Only the window is turned into quads; everything before it is skipped by scanning newlines,
    // and everything after it ends the pass.
    let first = placement.first_line as usize;
    let last = first + WINDOW as usize;

    // Token spans are sorted and non-overlapping, so one cursor over them classifies every byte.
    let mut spans = SpanCursor::new(&tile.spans);
    let mut run: Option<Run> = None;
    let mut line = 0usize;
    let mut col = 0usize;
    let mut baseline = baseline_of(0);

    for (offset, ch) in tile.text.char_indices() {
        if line < first {
            if ch == '\n' {
                line += 1;
                if line == first {
                    baseline = baseline_of(line);
                }
            }
            continue;
        }
        if ch == '\n' {
            flush(
                &mut strips,
                run.take(),
                x_left,
                advance,
                baseline,
                metrics,
                em,
                placement.z,
            );
            line += 1;
            if line >= last {
                break;
            }
            baseline = baseline_of(line);
            col = 0;
            continue;
        }
        if col >= MAX_COLS {
            continue; // the rest of a very long line is clipped, as in the grid measurement
        }
        if ch == '\t' || ch == ' ' || ch == '\r' {
            flush(
                &mut strips,
                run.take(),
                x_left,
                advance,
                baseline,
                metrics,
                em,
                placement.z,
            );
            col = if ch == '\t' {
                (col / TAB + 1) * TAB
            } else {
                col + 1
            };
            continue;
        }
        let class = spans.class_at(offset as u32);
        let color = palette.class(class);

        if glyphs.len() < MAX_GLYPHS {
            if let Some(cell) = atlas.cell(ch) {
                glyphs.push(GlyphInstance {
                    rect: [
                        x_left + col as f32 * advance + metrics.origin_x * em,
                        baseline + metrics.origin_y * em,
                        metrics.quad_w * em,
                        metrics.quad_h * em,
                    ],
                    z: placement.z,
                    cell,
                    color,
                    _pad: 0,
                });
            }
        }

        match &mut run {
            Some(r) if r.class == class && r.end == col => r.end = col + 1,
            other => {
                flush(
                    &mut strips,
                    other.take(),
                    x_left,
                    advance,
                    baseline,
                    metrics,
                    em,
                    placement.z,
                );
                *other = Some(Run {
                    class,
                    start: col,
                    end: col + 1,
                    color,
                });
            }
        }
        col += 1;
    }
    flush(
        &mut strips,
        run.take(),
        x_left,
        advance,
        baseline,
        metrics,
        em,
        placement.z,
    );

    let mut instances = Vec::with_capacity(glyphs.len() + strips.len() + 2);
    instances.push(backing);
    let glyph_count = glyphs.len() as u32;
    instances.append(&mut glyphs);
    instances.push(backing);
    let strip_count = strips.len() as u32;
    instances.append(&mut strips);

    PreparedText {
        file_id: placement.file_id,
        instances,
        glyph_count,
        strip_count,
        first_line: placement.first_line,
        line_world: metrics.line_height * em,
    }
}

/// A run of adjacent characters on one line sharing a token class, drawn as one bar.
struct Run {
    class: u8,
    start: usize,
    end: usize,
    color: u32,
}

#[allow(clippy::too_many_arguments)]
fn flush(
    out: &mut Vec<GlyphInstance>,
    run: Option<Run>,
    x_left: f32,
    advance: f32,
    baseline: f32,
    metrics: &Metrics,
    em: f32,
    z: f32,
) {
    let Some(run) = run else { return };
    if out.len() >= MAX_STRIPS {
        return;
    }
    out.push(GlyphInstance {
        rect: [
            x_left + run.start as f32 * advance,
            baseline,
            (run.end - run.start) as f32 * advance,
            metrics.line_height * em * STRIP_HEIGHT,
        ],
        z,
        cell: SOLID,
        color: run.color,
        _pad: 0,
    });
}

/// Walks sorted, non-overlapping token spans alongside a forward scan of the text.
struct SpanCursor<'a> {
    spans: &'a [TokenSpan],
    at: usize,
}

impl<'a> SpanCursor<'a> {
    fn new(spans: &'a [TokenSpan]) -> Self {
        SpanCursor { spans, at: 0 }
    }
    /// Class of the span covering `offset`, or 0 (`Other`) if none does. `offset` must not move
    /// backwards between calls.
    fn class_at(&mut self, offset: u32) -> u8 {
        while let Some(span) = self.spans.get(self.at) {
            if offset >= span.start + span.len {
                self.at += 1;
            } else if offset >= span.start {
                return span.class;
            } else {
                return 0;
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::Atlas;

    fn tile(text: &str, spans: Vec<TokenSpan>) -> TextTile {
        TextTile {
            file_id: 7,
            text: text.into(),
            spans,
        }
    }

    fn place() -> Placement {
        Placement {
            file_id: 7,
            rect: [10.0, 20.0, 4.0, 3.0],
            z: 1.5,
            first_line: 0,
        }
    }

    #[test]
    fn tiers_follow_the_spec_thresholds() {
        assert_eq!(tier(0.5), Tier::None);
        assert_eq!(tier(1.0), Tier::Strips);
        assert_eq!(tier(5.9), Tier::Strips);
        assert_eq!(tier(6.0), Tier::Glyphs);
        assert_eq!(tier(40.0), Tier::Glyphs);
    }

    #[test]
    fn the_grid_is_the_longest_line_by_the_line_count() {
        assert_eq!(grid_of("ab\ncdef\ng"), (4, 3));
        assert_eq!(grid_of("\tx"), (5, 1)); // tab to column 4, then one char
        assert_eq!(grid_of(""), (1, 1));
        // Long lines are clipped rather than shrinking the whole grid.
        let long = "x".repeat(400);
        assert_eq!(grid_of(&long).0, MAX_COLS);
    }

    #[test]
    fn every_glyph_quad_stays_inside_the_roof_rectangle() {
        let m = Atlas::bundled().metrics;
        let t = tile("fn main() {\n    let x = 1;\n}\n", vec![]);
        let p = place();
        let out = layout(&m, &t, p, &Palette::default());
        assert!(out.glyph_count > 0);
        // Quads are padded for the SDF, so allow the padding to spill past the text block but not
        // past the rectangle itself.
        for g in
            &out.instances[out.glyph_range().start as usize + 1..out.glyph_range().end as usize]
        {
            assert!(g.rect[0] >= p.rect[0] - 0.001, "left {:?}", g.rect);
            assert!(
                g.rect[0] + g.rect[2] <= p.rect[0] + p.rect[2] + 0.001,
                "right {:?}",
                g.rect
            );
            assert!(g.rect[1] >= p.rect[1] - 0.001, "bottom {:?}", g.rect);
            assert!(
                g.rect[1] + g.rect[3] <= p.rect[1] + p.rect[3] + 0.001,
                "top {:?}",
                g.rect
            );
            assert_eq!(g.z, p.z);
        }
    }

    #[test]
    fn both_tiers_are_contiguous_and_each_starts_with_a_backing_quad() {
        let m = Atlas::bundled().metrics;
        let t = tile(
            "let x = 1;\n",
            vec![
                TokenSpan {
                    start: 0,
                    len: 3,
                    class: 1,
                },
                TokenSpan {
                    start: 4,
                    len: 1,
                    class: 7,
                },
                TokenSpan {
                    start: 8,
                    len: 1,
                    class: 4,
                },
            ],
        );
        let out = layout(&m, &t, place(), &Palette::default());
        let g = out.glyph_range();
        let s = out.strip_range();
        assert_eq!(g.end, s.start);
        assert_eq!(s.end as usize, out.instances.len());
        assert_eq!(out.instances[g.start as usize].cell, SOLID);
        assert_eq!(out.instances[s.start as usize].cell, SOLID);
        assert!(
            out.strip_count >= 3,
            "one bar per token run: {}",
            out.strip_count
        );
        // Bars are solid; glyphs are not.
        assert!(out.instances[s.start as usize + 1..]
            .iter()
            .all(|i| i.cell == SOLID));
        assert!(out.instances[g.start as usize + 1..g.end as usize]
            .iter()
            .all(|i| i.cell != SOLID));
    }

    #[test]
    fn spans_color_the_characters_they_cover() {
        let m = Atlas::bundled().metrics;
        let palette = Palette::default();
        // "ab" is a keyword, "cd" is unclassified.
        let t = tile(
            "abcd",
            vec![TokenSpan {
                start: 0,
                len: 2,
                class: 1,
            }],
        );
        let out = layout(&m, &t, place(), &palette);
        let glyphs = &out.instances[1..1 + out.glyph_count as usize];
        assert_eq!(glyphs.len(), 4);
        assert_eq!(glyphs[0].color, palette.class(1));
        assert_eq!(glyphs[1].color, palette.class(1));
        assert_eq!(glyphs[2].color, palette.class(0));
        assert_eq!(glyphs[3].color, palette.class(0));
        // One run each: keyword then other.
        assert_eq!(out.strip_count, 2);
    }

    #[test]
    fn whitespace_draws_nothing_and_breaks_runs() {
        let m = Atlas::bundled().metrics;
        let t = tile(
            "a b",
            vec![TokenSpan {
                start: 0,
                len: 3,
                class: 1,
            }],
        );
        let out = layout(&m, &t, place(), &Palette::default());
        assert_eq!(out.glyph_count, 2, "the space has no glyph");
        assert_eq!(out.strip_count, 2, "the space splits the run");
    }

    #[test]
    fn layout_is_deterministic() {
        let m = Atlas::bundled().metrics;
        let t = tile(
            "fn a() {}\nfn b() {}\n",
            vec![TokenSpan {
                start: 0,
                len: 2,
                class: 1,
            }],
        );
        assert_eq!(
            layout(&m, &t, place(), &Palette::default()),
            layout(&m, &t, place(), &Palette::default())
        );
    }

    #[test]
    fn an_empty_file_still_produces_both_backing_quads() {
        let m = Atlas::bundled().metrics;
        let out = layout(&m, &tile("", vec![]), place(), &Palette::default());
        assert_eq!(out.glyph_count, 0);
        assert_eq!(out.strip_count, 0);
        assert_eq!(out.instances.len(), 2);
    }

    #[test]
    fn a_huge_file_is_capped() {
        let m = Atlas::bundled().metrics;
        let text = "let value = compute(x);\n".repeat(8000);
        let out = layout(&m, &tile(&text, vec![]), place(), &Palette::default());
        assert!(out.glyph_count as usize <= MAX_GLYPHS);
        assert!(out.strip_count as usize <= MAX_STRIPS);
    }

    #[test]
    fn a_long_file_centres_its_block_on_the_roof() {
        // A 300-line file of short lines fits a wide roof only by shrinking until its lines fit
        // vertically, which leaves a narrow ribbon of code. Pinned to a corner, that ribbon sits
        // nowhere near where a camera aimed at the roof looks.
        let m = Atlas::bundled().metrics;
        let text = "let x = 1;\n".repeat(300);
        let p = Placement {
            file_id: 1,
            rect: [100.0, 200.0, 40.0, 10.0],
            z: 0.0,
            first_line: 0,
        };
        let out = layout(&m, &tile(&text, vec![]), p, &Palette::default());
        let glyphs = &out.instances[1..1 + out.glyph_count as usize];
        let (mut x0, mut x1) = (f32::MAX, f32::MIN);
        let (mut y0, mut y1) = (f32::MAX, f32::MIN);
        for g in glyphs {
            x0 = x0.min(g.rect[0]);
            x1 = x1.max(g.rect[0] + g.rect[2]);
            y0 = y0.min(g.rect[1]);
            y1 = y1.max(g.rect[1] + g.rect[3]);
        }
        // The block really is much narrower than the roof, which is what makes centring matter.
        assert!(x1 - x0 < p.rect[2] * 0.2, "block width {}", x1 - x0);
        let centre = |a: f32, b: f32| (a + b) * 0.5;
        assert!(
            (centre(x0, x1) - centre(p.rect[0], p.rect[0] + p.rect[2])).abs() < p.rect[2] * 0.02,
            "block x centre {} vs roof {}",
            centre(x0, x1),
            centre(p.rect[0], p.rect[0] + p.rect[2])
        );
        assert!(
            (centre(y0, y1) - centre(p.rect[1], p.rect[1] + p.rect[3])).abs() < p.rect[3] * 0.02,
            "block y centre {} vs roof {}",
            centre(y0, y1),
            centre(p.rect[1], p.rect[1] + p.rect[3])
        );
    }

    #[test]
    fn a_window_holds_its_slice_at_the_same_scale_as_every_other() {
        // A long file is laid out a window at a time, but the em size comes from the whole file:
        // if it did not, the text would change size as the camera moved down the roof.
        let m = Atlas::bundled().metrics;
        let text: String = (0..3000).map(|i| format!("let v{i} = {i};\n")).collect();
        let at = |first_line| {
            let mut p = place();
            p.rect = [0.0, 0.0, 60.0, 90.0];
            p.first_line = first_line;
            layout(&m, &tile(&text, vec![]), p, &Palette::default())
        };
        let top = at(0);
        let middle = at(1024);
        assert_eq!(top.line_world, middle.line_world);
        assert_eq!(middle.first_line, 1024);
        assert!(top.glyph_count > 0 && middle.glyph_count > 0);

        // The window really is a slice: 3000 lines do not fit in one layout.
        assert!(top.glyph_count < 3000 * 5);
        // And it sits exactly 1024 lines lower on the roof.
        let first_baseline = |o: &PreparedText| o.instances[1].rect[1];
        let drop = first_baseline(&top) - first_baseline(&middle);
        assert!(
            (drop - 1024.0 * top.line_world).abs() < top.line_world * 0.01,
            "window 1024 sits {drop} below window 0, expected {}",
            1024.0 * top.line_world
        );
    }

    #[test]
    fn windows_snap_so_small_moves_do_not_re_lay_out() {
        assert_eq!(Placement::window_for(0), 0);
        assert_eq!(Placement::window_for(100), 0);
        // Once past the step the window advances, and the asked-for line sits inside it.
        for line in [600u32, 1000, 5000, 12_345] {
            let start = Placement::window_for(line);
            assert!(start <= line, "{line} is before window {start}");
            assert!(line < start + WINDOW, "{line} is past window {start}");
        }
        // Every line in one step maps to the same window, so panning inside it costs nothing.
        for line in 1024..1024 + WINDOW_STEP {
            assert_eq!(Placement::window_for(line), Placement::window_for(1024));
        }
        // Crossing a step moves the window by exactly one step.
        assert_eq!(
            Placement::window_for(1024 + WINDOW_STEP) - Placement::window_for(1024),
            WINDOW_STEP
        );
    }

    #[test]
    fn line_height_shrinks_as_a_file_grows() {
        let m = Atlas::bundled().metrics;
        let small = line_world_height(&m, place().rect, grid_of("a\nb\n"));
        let big = line_world_height(&m, place().rect, grid_of(&"a\n".repeat(500)));
        assert!(big < small);
    }
}
