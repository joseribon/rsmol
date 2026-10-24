//! Per-atom geometry: van der Waals spheres and licorice sticks.

use bytemuck::{Pod, Zeroable};
use rsmol_core::{BondKind, Structure, Vec3};

use crate::style;

/// One sphere impostor: 20 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct SphereInstance {
    pub center: [f32; 3],
    pub radius: f32,
    pub color: [u8; 4],
}

/// One bond cylinder, colored by atom at each half: 36 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct StickInstance {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub radius: f32,
    pub color_a: [u8; 4],
    pub color_b: [u8; 4],
}

/// Licorice radius in Å, shared by bonds and their joints.
pub const STICK_RADIUS: f32 = 0.2;
/// Unbonded atoms (ions) in stick mode are drawn at this fraction of their vdW radius.
const ION_SCALE: f32 = 0.5;

/// Van der Waals spheres for the atoms in `include`.
pub fn spheres(s: &Structure, coords: &[Vec3], include: &[bool], color: impl Fn(usize) -> [u8; 4]) -> Vec<SphereInstance> {
    (0..coords.len())
        .filter(|&i| include[i])
        .map(|i| SphereInstance { center: coords[i], radius: style::vdw_radius(s.atoms.element[i]), color: color(i) })
        .collect()
}

/// Sticks for covalent bonds between atoms in `include`, a joint sphere on every bonded atom,
/// and a small sphere for included atoms without bonds (ions).
pub fn sticks(
    s: &Structure,
    coords: &[Vec3],
    include: &[bool],
    color: impl Fn(usize) -> [u8; 4],
) -> (Vec<SphereInstance>, Vec<StickInstance>) {
    let mut bonded = vec![false; coords.len()];
    let mut sticks = Vec::new();
    for bond in &s.bonds {
        // Metal coordination is not a covalent bond; it will get a dashed style later.
        if bond.kind == BondKind::MetalCoordination {
            continue;
        }
        let [a, b] = bond.atoms.map(|x| x as usize);
        if !(include[a] && include[b]) {
            continue;
        }
        bonded[a] = true;
        bonded[b] = true;
        sticks.push(StickInstance {
            a: coords[a],
            b: coords[b],
            radius: STICK_RADIUS,
            color_a: color(a),
            color_b: color(b),
        });
    }
    let spheres = (0..coords.len())
        .filter(|&i| include[i])
        .map(|i| {
            let radius = if bonded[i] { STICK_RADIUS } else { style::vdw_radius(s.atoms.element[i]) * ION_SCALE };
            SphereInstance { center: coords[i], radius, color: color(i) }
        })
        .collect();
    (spheres, sticks)
}
