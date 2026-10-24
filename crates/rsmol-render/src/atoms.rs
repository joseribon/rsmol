//! Per-atom geometry: van der Waals spheres and licorice sticks, built from the display state.

use bytemuck::{Pod, Zeroable};
use rsmol_core::display::{Display, Reps};
use rsmol_core::{BondKind, Structure, Vec3};

use crate::style;

/// One sphere impostor: 24 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct SphereInstance {
    pub center: [f32; 3],
    pub radius: f32,
    pub color: [u8; 4],
    /// Atom index + 1 for picking; 0 picks nothing.
    pub id: u32,
}

/// One bond cylinder, colored by atom at each half: 44 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct StickInstance {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub radius: f32,
    pub color_a: [u8; 4],
    pub color_b: [u8; 4],
    pub id_a: u32,
    pub id_b: u32,
}

/// The pick id of atom `i`.
pub fn pick_id(i: usize) -> u32 {
    i as u32 + 1
}

/// Licorice radius in Å, shared by bonds and their joints.
pub const STICK_RADIUS: f32 = 0.2;
/// Unbonded atoms (ions) in stick mode are drawn at this fraction of their vdW radius.
const ION_SCALE: f32 = 0.5;

/// Van der Waals spheres for atoms shown as spheres.
pub fn spheres(s: &Structure, coords: &[Vec3], display: &Display) -> Vec<SphereInstance> {
    (0..coords.len())
        .filter(|&i| display.reps[i].contains(Reps::SPHERES))
        .map(|i| SphereInstance {
            center: coords[i],
            radius: style::vdw_radius(s.atoms.element[i]),
            color: display.colors[i],
            id: pick_id(i),
        })
        .collect()
}

/// Sticks for covalent bonds between atoms shown as sticks, a joint sphere on every such atom,
/// and a small sphere for stick atoms without bonds (ions).
///
/// `anchors` are atoms that sticks may attach to without being sticks themselves: the cartoon's
/// backbone guide atoms, so side chains shown as sticks connect to the ribbon.
pub fn sticks(s: &Structure, coords: &[Vec3], display: &Display, anchors: &[bool]) -> (Vec<SphereInstance>, Vec<StickInstance>) {
    let is_stick = |i: usize| display.reps[i].contains(Reps::STICKS);
    // Only atoms without any covalent bond (ions) get the larger sphere; a bonded atom whose
    // partners are hidden (a glycine CA shown alone) still gets a joint-sized one.
    let mut bonded = vec![false; coords.len()];
    for bond in s.bonds.iter().filter(|b| b.kind != BondKind::MetalCoordination) {
        bonded[bond.atoms[0] as usize] = true;
        bonded[bond.atoms[1] as usize] = true;
    }
    let mut sticks = Vec::new();
    for bond in &s.bonds {
        // Metal coordination is not a covalent bond; it will get a dashed style later.
        if bond.kind == BondKind::MetalCoordination {
            continue;
        }
        let [a, b] = bond.atoms.map(|x| x as usize);
        let drawn = (is_stick(a) && (is_stick(b) || anchors[b])) || (is_stick(b) && anchors[a]);
        if !drawn {
            continue;
        }
        sticks.push(StickInstance {
            a: coords[a],
            b: coords[b],
            radius: STICK_RADIUS,
            color_a: display.colors[a],
            color_b: display.colors[b],
            id_a: pick_id(a),
            id_b: pick_id(b),
        });
    }
    let spheres = (0..coords.len())
        .filter(|&i| is_stick(i))
        .map(|i| {
            let radius = if bonded[i] { STICK_RADIUS } else { style::vdw_radius(s.atoms.element[i]) * ION_SCALE };
            SphereInstance { center: coords[i], radius, color: display.colors[i], id: pick_id(i) }
        })
        .collect();
    (spheres, sticks)
}
