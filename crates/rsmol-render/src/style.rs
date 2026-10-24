//! Default colors and radii.

use rsmol_core::Element;

/// Van der Waals radius in Å (Bondi 1964, with common defaults for metals).
pub fn vdw_radius(e: Element) -> f32 {
    match e.atomic_number() {
        1 => 1.10,
        6 => 1.70,
        7 => 1.55,
        8 => 1.52,
        9 => 1.47,
        15 => 1.80,
        16 => 1.80,
        17 => 1.75,
        34 => 1.90,
        35 => 1.85,
        53 => 1.98,
        11 | 12 | 19 | 20 | 25..=30 => 1.40,
        _ => 1.70,
    }
}

/// Heteroatom colors as sRGB; carbons are colored by chain instead.
pub fn element_color(e: Element) -> Option<[u8; 4]> {
    Some(match e.atomic_number() {
        1 => [235, 235, 235, 255],
        7 => [72, 110, 230, 255],
        8 => [230, 60, 50, 255],
        9 | 17 => [120, 210, 90, 255],
        15 => [255, 140, 40, 255],
        16 => [240, 200, 50, 255],
        26 => [210, 110, 50, 255],
        30 => [130, 130, 160, 255],
        12 | 20 => [140, 220, 140, 255],
        _ => return None,
    })
}

/// Ligand carbons: a light neutral that reads against every chain color.
pub const LIGAND_CARBON: [u8; 4] = [214, 218, 226, 255];

/// Muted, well-separated hues for carbons, cycled per chain.
const CHAIN_PALETTE: [[u8; 4]; 10] = [
    [96, 186, 168, 255],
    [232, 160, 92, 255],
    [138, 152, 220, 255],
    [214, 122, 166, 255],
    [160, 196, 98, 255],
    [228, 200, 104, 255],
    [118, 176, 214, 255],
    [192, 142, 112, 255],
    [170, 130, 200, 255],
    [150, 150, 150, 255],
];

pub fn chain_color(index: usize) -> [u8; 4] {
    CHAIN_PALETTE[index % CHAIN_PALETTE.len()]
}
