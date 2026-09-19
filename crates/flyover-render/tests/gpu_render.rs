//! End-to-end GPU check: render a real tile set offscreen, confirm pixels were drawn, and pick
//! the file under the center pixel. Skips (passes with a note) on machines without a GPU adapter.

mod common;

use flyover_render::{screenshot, Error, ViewOptions};

#[test]
fn renders_a_tile_set_and_picks_a_file() {
    let dir = common::tileset("gpu");
    let png = dir.join("shot.png");
    let shot = match screenshot(&dir, &png, 320, 200, 0.0, &ViewOptions::default()) {
        Ok(shot) => shot,
        Err(Error::Adapter(e)) => {
            eprintln!("no GPU adapter ({e}); skipping");
            return;
        }
        Err(e) => panic!("render failed: {e}"),
    };

    assert!(shot.settled, "every wanted tile loaded");
    assert!(shot.drawn_tiles > 0);
    assert!(
        shot.coverage > 0.1,
        "only {:.1}% of pixels drawn",
        shot.coverage * 100.0
    );
    assert!(png.exists());

    let (id, what) = shot.center;
    let (path, _) = what.unwrap_or_else(|| panic!("center pixel (id {id}) is not a file"));
    assert!(path.starts_with("src/mod"), "unexpected center path {path}");
}
