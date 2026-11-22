//! Material 3 colour roles generated from one seed colour, emitted as GTK CSS
//! custom properties. The stylesheet only ever uses `var(--role)`.

/// Accent presets offered in the menu (first is the default lavender).
pub const PRESETS: &[&str] = &["#6750A4", "#0061A4", "#006E1C", "#9C4146", "#8B5000", "#006A6A"];

pub fn parse_hex(s: &str) -> Option<(f64, f64, f64)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    let c = |shift: u32| f64::from((v >> shift) & 0xff) / 255.0;
    Some((c(16), c(8), c(0)))
}

fn rgb_to_hsl((r, g, b): (f64, f64, f64)) -> (f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let l = (max + min) / 2.0;
    if d == 0.0 {
        return (0.0, 0.0);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s)
}

/// Colour at a Material-style tone (0 black .. 100 white).
// shortcut: HSL lightness stands in for HCT tone, so contrast is close to but
// not exactly Material's; port material-color-utilities if exact tones matter.
fn tone(h: f64, s: f64, t: f64) -> String {
    let l = t / 100.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s.clamp(0.0, 1.0);
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f64| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", to(r), to(g), to(b))
}

/// (role, light tone, dark tone, palette): 0 primary, 1 secondary, 2 tertiary, 3 neutral, 4 neutral-variant.
const ROLES: &[(&str, f64, f64, usize)] = &[
    ("primary", 40.0, 80.0, 0),
    ("on-primary", 100.0, 20.0, 0),
    ("primary-container", 90.0, 30.0, 0),
    ("on-primary-container", 10.0, 90.0, 0),
    ("secondary", 40.0, 80.0, 1),
    ("on-secondary", 100.0, 20.0, 1),
    ("secondary-container", 90.0, 30.0, 1),
    ("on-secondary-container", 10.0, 90.0, 1),
    ("tertiary", 40.0, 80.0, 2),
    ("on-tertiary", 100.0, 20.0, 2),
    ("tertiary-container", 90.0, 30.0, 2),
    ("on-tertiary-container", 10.0, 90.0, 2),
    ("surface", 98.0, 6.0, 3),
    ("surface-container-lowest", 100.0, 4.0, 3),
    ("surface-container-low", 96.0, 10.0, 3),
    ("surface-container", 94.0, 12.0, 3),
    ("surface-container-high", 92.0, 17.0, 3),
    ("surface-container-highest", 90.0, 22.0, 3),
    ("on-surface", 10.0, 90.0, 3),
    ("inverse-surface", 20.0, 90.0, 3),
    ("inverse-on-surface", 95.0, 20.0, 3),
    ("on-surface-variant", 30.0, 80.0, 4),
    ("outline", 50.0, 60.0, 4),
    ("outline-variant", 80.0, 30.0, 4),
];

fn block(seed: (f64, f64, f64), dark: bool) -> String {
    let (h, s) = rgb_to_hsl(seed);
    let s = s.max(0.35);
    let palettes = [(h, s), (h, s * 0.3), (h + 60.0, s * 0.45), (h, 0.06), (h, 0.12)];
    let mut out = String::new();
    for &(name, light_t, dark_t, p) in ROLES {
        let (ph, ps) = palettes[p];
        let t = if dark { dark_t } else { light_t };
        out.push_str(&format!("  --{name}: {};\n", tone(ph, ps, t)));
    }
    let (err, on_err) = if dark {
        ("#ffb4ab", "#690005")
    } else {
        ("#ba1a1a", "#ffffff")
    };
    out.push_str(&format!("  --error: {err};\n  --on-error: {on_err};\n"));
    out
}

/// Token CSS for one scheme. (GTK 4.22 ignores custom properties declared
/// inside `@media (prefers-color-scheme)`, so the scheme is chosen in Rust.)
pub fn css(accent: &str, dark: bool) -> String {
    let seed = parse_hex(accent)
        .or_else(|| parse_hex(PRESETS[0]))
        .unwrap_or((0.4, 0.3, 0.64));
    let mut css = format!(":root {{\n{}}}\n", block(seed, dark));
    for (i, p) in PRESETS.iter().enumerate() {
        css.push_str(&format!(".swatch-{i} {{ background: {p}; }}\n"));
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex() {
        assert_eq!(parse_hex("#ff0000"), Some((1.0, 0.0, 0.0)));
        assert_eq!(parse_hex("ff0000"), None);
        assert_eq!(parse_hex("#ff00"), None);
    }

    #[test]
    fn tones_are_ordered() {
        assert_eq!(tone(270.0, 0.5, 100.0), "#ffffff");
        assert_eq!(tone(270.0, 0.5, 0.0), "#000000");
        let css = css("#6750A4", false);
        assert!(css.contains("--surface-container-high:"));
        assert_ne!(css, super::css("#6750A4", true));
        // invalid accent falls back instead of producing broken CSS
        assert_eq!(super::css("nope", false), css);
    }
}
