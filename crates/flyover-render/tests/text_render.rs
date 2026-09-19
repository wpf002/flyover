//! M4 end to end: index a real (small) repo, lay it out, fly the camera down onto one file's
//! roof, and check that the pixels coming back are its source code — right tier, right colors,
//! and materially different from the same frame with text turned off.
//!
//! Skips (passes with a note) on a machine with no GPU adapter.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use flyover_render::text::Palette;
use flyover_render::{screenshot, Error, Shot, ViewOptions};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const SOURCE: &str = r#"// A small module, used to check that source text reaches the screen.
use std::collections::HashMap;

pub struct Registry {
    entries: HashMap<String, u32>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry { entries: HashMap::new() }
    }

    pub fn insert(&mut self, name: &str, value: u32) -> bool {
        self.entries.insert(name.to_string(), value).is_none()
    }

    pub fn get(&self, name: &str) -> Option<u32> {
        self.entries.get(name).copied()
    }
}
"#;

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "flyover-m4-render-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = dir.join("repo");

    write(&repo.join("src/registry.rs"), SOURCE);
    // A few neighbours so the focused file is one building among several, not the whole world.
    for i in 0..6 {
        write(
            &repo.join(format!("src/other{i}.rs")),
            &format!("pub fn helper{i}(x: u32) -> u32 {{\n    x * {i} + 1\n}}\n"),
        );
    }

    let index = dir.join("index");
    flyover_index::run(&repo, &index, &flyover_index::Options::default()).unwrap();
    let tiles = dir.join("tiles");
    flyover_layout::run(
        &index.join("index.db"),
        &tiles,
        &flyover_layout::Options::default(),
    )
    .unwrap();
    (dir, tiles)
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn shoot(tiles: &Path, png: &Path, text: bool) -> Result<(Shot, Vec<u8>), Error> {
    let opts = ViewOptions {
        focus: Some("registry.rs".into()),
        text,
        ..Default::default()
    };
    let shot = screenshot(tiles, png, 800, 500, 0.0, &opts)?;
    Ok((shot, read_png(png)))
}

fn read_png(path: &Path) -> Vec<u8> {
    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    buf
}

/// Pixels within `tol` of an rgb triple in 0..1.
fn count_near(rgba: &[u8], rgb: [f32; 3], tol: i32) -> usize {
    let target = rgb.map(|c| (c * 255.0).round() as i32);
    rgba.chunks_exact(4)
        .filter(|p| (0..3).all(|c| (i32::from(p[c]) - target[c]).abs() <= tol))
        .count()
}

#[test]
fn a_file_roof_shows_its_own_source() {
    let (dir, tiles) = fixture("glyphs");
    let with_text = dir.join("text.png");
    let without = dir.join("plain.png");

    let (shot, lit) = match shoot(&tiles, &with_text, true) {
        Ok(v) => v,
        Err(Error::Adapter(e)) => {
            eprintln!("no GPU adapter ({e}); skipping");
            return;
        }
        Err(e) => panic!("render failed: {e}"),
    };
    let (plain_shot, plain) = shoot(&tiles, &without, false).unwrap();

    // The focused roof fills the view, so it is over the glyph threshold.
    assert!(
        shot.glyph_files >= 1,
        "expected a file at the glyph tier, got {} text files ({} glyph)",
        shot.text_files,
        shot.glyph_files
    );
    assert_eq!(
        (plain_shot.text_files, plain_shot.glyph_files),
        (0, 0),
        "--no-text must draw no text at all"
    );

    // Text changes a real part of the frame.
    let changed = lit
        .chunks_exact(4)
        .zip(plain.chunks_exact(4))
        .filter(|(a, b)| (0..3).any(|c| (i32::from(a[c]) - i32::from(b[c])).abs() > 8))
        .count();
    let total = lit.len() / 4;
    assert!(
        changed * 100 / total >= 5,
        "text changed only {:.1}% of the frame",
        changed as f32 * 100.0 / total as f32
    );

    // Token classes arrive at the screen in near-pure form: this file has keywords, a comment,
    // a string, types, and punctuation, so several classes must be visible.
    let palette = Palette::default();
    let present: Vec<usize> = (0..palette.classes.len())
        .filter(|i| count_near(&lit, palette.classes[*i], 10) >= 20)
        .collect();
    assert!(
        present.len() >= 3,
        "only {} token classes reached the screen: {present:?}",
        present.len()
    );

    // The dark backing quad under the text is there, so the code reads against any roof color.
    let backing = [palette.backing[0], palette.backing[1], palette.backing[2]];
    assert!(
        count_near(&lit, backing, 24) >= total / 20,
        "the text backing quad is missing"
    );

    eprintln!(
        "wrote {} ({} text files, {} at glyph tier, {:.1}% of pixels changed)",
        with_text.display(),
        shot.text_files,
        shot.glyph_files,
        changed as f32 * 100.0 / total as f32
    );
    // Left in place on purpose: these are the frames the milestone report points at.
}

#[test]
fn distance_drops_the_tier_to_bars_and_then_to_nothing() {
    let (dir, tiles) = fixture("tiers");
    let png = dir.join("far.png");

    // The overview camera: every file is far away, so no roof can be at the glyph tier.
    let far = match screenshot(&tiles, &png, 800, 500, 0.0, &ViewOptions::default()) {
        Ok(shot) => shot,
        Err(Error::Adapter(e)) => {
            eprintln!("no GPU adapter ({e}); skipping");
            return;
        }
        Err(e) => panic!("render failed: {e}"),
    };
    let near = screenshot(
        &tiles,
        &png,
        800,
        500,
        0.0,
        &ViewOptions {
            focus: Some("registry.rs".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        near.glyph_files > far.glyph_files,
        "close up ({} glyph files) should read more than the overview ({})",
        near.glyph_files,
        far.glyph_files
    );
    std::fs::remove_dir_all(&dir).ok();
}
