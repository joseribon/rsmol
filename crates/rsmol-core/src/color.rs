//! Colors: named palette, element and chain defaults, and data-driven schemes.
//!
//! Names follow PyMOL's vocabulary, but the shades are tuned for dark backgrounds and modern
//! displays rather than raw RGB primaries.

use crate::{Element, SecondaryStructure};

/// sRGB color with alpha, 8 bits per channel.
pub type Rgba = [u8; 4];

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    [r, g, b, 255]
}

/// Named colors, alphabetical.
pub const NAMED: &[(&str, Rgba)] = &[
    ("amber", rgb(245, 180, 60)),
    ("black", rgb(28, 30, 34)),
    ("blue", rgb(70, 120, 230)),
    ("brown", rgb(150, 100, 70)),
    ("coral", rgb(240, 128, 110)),
    ("cyan", rgb(57, 197, 207)),
    ("forest", rgb(52, 130, 80)),
    ("gold", rgb(222, 180, 70)),
    ("gray", rgb(150, 155, 165)),
    ("green", rgb(87, 171, 90)),
    ("grey", rgb(150, 155, 165)),
    ("lavender", rgb(180, 160, 230)),
    ("lime", rgb(170, 210, 80)),
    ("magenta", rgb(214, 90, 180)),
    ("marine", rgb(60, 110, 190)),
    ("mint", rgb(140, 214, 180)),
    ("navy", rgb(40, 60, 120)),
    ("olive", rgb(150, 160, 70)),
    ("orange", rgb(240, 136, 62)),
    ("pink", rgb(240, 140, 180)),
    ("purple", rgb(152, 110, 220)),
    ("red", rgb(229, 83, 75)),
    ("rose", rgb(226, 110, 140)),
    ("salmon", rgb(244, 150, 130)),
    ("sand", rgb(214, 190, 150)),
    ("sky", rgb(118, 176, 214)),
    ("slate", rgb(112, 128, 160)),
    ("teal", rgb(96, 186, 168)),
    ("violet", rgb(138, 90, 200)),
    ("wheat", rgb(236, 212, 160)),
    ("white", rgb(236, 238, 242)),
    ("yellow", rgb(234, 196, 53)),
];

/// A named color (case-insensitive) or `#rrggbb` / `0xrrggbb`.
pub fn parse(text: &str) -> Option<Rgba> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix('#').or_else(|| t.strip_prefix("0x")) {
        if hex.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    NAMED.iter().find(|(name, _)| name.eq_ignore_ascii_case(t)).map(|&(_, c)| c)
}

/// Heteroatom colors; `None` for carbon (colored by chain or scheme instead).
pub fn element(e: Element) -> Option<Rgba> {
    Some(match e.atomic_number() {
        1 => rgb(235, 235, 235),
        6 => return None,
        7 => rgb(72, 110, 230),
        8 => rgb(230, 60, 50),
        9 | 17 => rgb(120, 210, 90),
        15 => rgb(255, 140, 40),
        16 => rgb(240, 200, 50),
        26 => rgb(210, 110, 50),
        30 => rgb(130, 130, 160),
        12 | 20 => rgb(140, 220, 140),
        35 => rgb(166, 41, 41),
        53 => rgb(148, 0, 148),
        _ => rgb(200, 160, 200),
    })
}

/// Ligand carbons: a light neutral that reads against every chain color.
pub const LIGAND_CARBON: Rgba = rgb(214, 218, 226);

/// Muted, well-separated hues, cycled per chain.
pub const CHAIN_PALETTE: [Rgba; 10] = [
    rgb(96, 186, 168),
    rgb(232, 160, 92),
    rgb(138, 152, 220),
    rgb(214, 122, 166),
    rgb(160, 196, 98),
    rgb(228, 200, 104),
    rgb(118, 176, 214),
    rgb(192, 142, 112),
    rgb(170, 130, 200),
    rgb(150, 150, 150),
];

pub fn chain(index: usize) -> Rgba {
    CHAIN_PALETTE[index % CHAIN_PALETTE.len()]
}

pub fn secondary_structure(ss: SecondaryStructure) -> Rgba {
    match ss {
        SecondaryStructure::Helix => rgb(229, 111, 134),
        SecondaryStructure::Sheet => rgb(240, 196, 84),
        SecondaryStructure::Loop => rgb(170, 175, 185),
    }
}

/// Diverging blue → white → red ramp (Moreland's "cool to warm"), `t` in [0, 1].
pub fn spectrum(t: f32) -> Rgba {
    const STOPS: [[f32; 3]; 3] = [[59.0, 76.0, 192.0], [221.0, 221.0, 221.0], [180.0, 4.0, 38.0]];
    let t = t.clamp(0.0, 1.0) * 2.0;
    let (a, b, f) = if t < 1.0 { (STOPS[0], STOPS[1], t) } else { (STOPS[1], STOPS[2], t - 1.0) };
    let mix = |k: usize| (a[k] + (b[k] - a[k]) * f).round() as u8;
    rgb(mix(0), mix(1), mix(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_hex() {
        assert_eq!(parse("Teal"), Some(rgb(96, 186, 168)));
        assert_eq!(parse("#ff8000"), Some(rgb(255, 128, 0)));
        assert_eq!(parse("0x0000FF"), Some(rgb(0, 0, 255)));
        assert_eq!(parse("#fff"), None);
        assert_eq!(parse("chartreuse"), None);
    }

    #[test]
    fn named_colors_are_sorted_for_listing() {
        assert!(NAMED.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn spectrum_endpoints() {
        assert_eq!(spectrum(0.0), rgb(59, 76, 192));
        assert_eq!(spectrum(0.5), rgb(221, 221, 221));
        assert_eq!(spectrum(1.0), rgb(180, 4, 38));
    }
}
