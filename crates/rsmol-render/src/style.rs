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

/// Selection markers: PyMOL's familiar pink, drawn as small overlay spheres.
pub const SELECTION: [u8; 4] = [255, 92, 200, 255];
pub const MARKER_RADIUS: f32 = 0.3;
