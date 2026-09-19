//! Deterministic language -> color. Common languages use fixed, Linguist-like colors; anything
//! else gets a stable color derived from a hash of its name, so the same language is always the
//! same color across repos and runs.

const KNOWN: &[(&str, &str)] = &[
    ("C", "#555555"),
    ("C++", "#f34b7d"),
    ("C#", "#178600"),
    ("CSS", "#563d7c"),
    ("Go", "#00add8"),
    ("HTML", "#e34c26"),
    ("Java", "#b07219"),
    ("JavaScript", "#f1e05a"),
    ("JSON", "#969696"),
    ("Kotlin", "#a97bff"),
    ("Markdown", "#083fa1"),
    ("Objective-C", "#438eff"),
    ("PHP", "#4f5d95"),
    ("Python", "#3572a5"),
    ("Ruby", "#701516"),
    ("Rust", "#dea584"),
    ("Shell", "#89e051"),
    ("SQL", "#e38c00"),
    ("Swift", "#f05138"),
    ("TOML", "#9c4221"),
    ("TypeScript", "#3178c6"),
    ("YAML", "#cb171e"),
    ("Other", "#8a929c"),
];

/// A hex color (`#rrggbb`) for a language name.
pub fn color_for(language: &str) -> String {
    if let Some((_, hex)) = KNOWN.iter().find(|(name, _)| *name == language) {
        return (*hex).to_string();
    }
    // Stable hash -> hue. Fixed saturation/lightness keep colors legible on the dark map.
    let hue = fnv1a(language) % 360;
    hsl_to_hex(hue as f64, 0.55, 0.6)
}

fn fnv1a(s: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn hsl_to_hex(h: f64, s: f64, l: f64) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", to(r1), to(g1), to(b1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_languages_are_fixed() {
        assert_eq!(color_for("Rust"), "#dea584");
        assert_eq!(color_for("TypeScript"), "#3178c6");
    }

    #[test]
    fn unknown_is_stable_and_valid_hex() {
        let a = color_for("Brainfuck");
        let b = color_for("Brainfuck");
        assert_eq!(a, b);
        assert_eq!(a.len(), 7);
        assert!(a.starts_with('#'));
        assert!(a[1..].chars().all(|c| c.is_ascii_hexdigit()));
    }
}
