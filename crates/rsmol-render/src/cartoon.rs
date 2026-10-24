//! Cartoon geometry: a Catmull-Rom spline through each backbone trace, swept with an elliptical
//! cross-section that is flat for helices and strands, round for loops and nucleic acids, and
//! ends each strand in an arrowhead.

use std::f32::consts::TAU;

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use rsmol_core::backbone::{PolymerKind, Trace};
use rsmol_core::{SecondaryStructure as Ss, Structure};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [u8; 4],
    /// Pick id of the residue's guide atom.
    pub id: u32,
}

#[derive(Default)]
pub struct Mesh {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
}

const SAMPLES_PER_RESIDUE: usize = 6;
const RING: usize = 12;

/// Cross-section half extents in Å: across the ribbon, and through it.
#[derive(Clone, Copy)]
struct Profile {
    width: f32,
    thickness: f32,
}

const LOOP: Profile = Profile { width: 0.3, thickness: 0.3 };
const HELIX: Profile = Profile { width: 1.3, thickness: 0.28 };
const SHEET: Profile = Profile { width: 1.1, thickness: 0.28 };
const NUCLEIC: Profile = Profile { width: 0.55, thickness: 0.55 };
const ARROW_WIDTH: f32 = 1.75;

impl Profile {
    fn lerp(self, other: Profile, t: f32) -> Profile {
        Profile {
            width: self.width + (other.width - self.width) * t,
            thickness: self.thickness + (other.thickness - self.thickness) * t,
        }
    }
}

/// Builds one mesh for all `traces`; `color` gives each residue's color.
pub fn build(s: &Structure, state: usize, traces: &[Trace], color: impl Fn(u32) -> [u8; 4]) -> Mesh {
    let mut mesh = Mesh::default();
    let Some(coords) = s.coords(state) else { return mesh };
    for trace in traces {
        sweep(&mut mesh, coords, s, trace, &color);
    }
    mesh
}

fn sweep(mesh: &mut Mesh, coords: &[[f32; 3]], s: &Structure, trace: &Trace, color: &impl Fn(u32) -> [u8; 4]) {
    let n = trace.len();
    let at = |atom: u32| Vec3::from(coords[atom as usize]);
    let ss: Vec<Ss> = match trace.kind {
        PolymerKind::Protein => trace.secondary_structure(s).collect(),
        PolymerKind::Nucleic => vec![Ss::Loop; n],
    };

    // Strands pleat: CA atoms zigzag around the strand axis. Averaging interior strand points
    // with their neighbours straightens the ribbon, as most viewers do.
    let raw: Vec<Vec3> = trace.guide_atoms.iter().map(|&a| at(a)).collect();
    let mut points = raw.clone();
    for i in 1..n.saturating_sub(1) {
        if ss[i - 1] == Ss::Sheet && ss[i] == Ss::Sheet && ss[i + 1] == Ss::Sheet {
            points[i] = raw[i - 1] * 0.25 + raw[i] * 0.5 + raw[i + 1] * 0.25;
        }
    }

    // Ribbon width direction per residue from CA→O. Carbonyls alternate sides along a strand,
    // so flip each to agree with its predecessor or the ribbon would twist 180° per residue.
    let mut dirs: Vec<Option<Vec3>> = trace
        .guide_atoms
        .iter()
        .zip(&trace.orient_atoms)
        .map(|(&ca, o)| o.map(|o| (at(o) - at(ca)).normalize_or_zero()).filter(|d| *d != Vec3::ZERO))
        .collect();
    let mut prev: Option<Vec3> = None;
    for d in dirs.iter_mut().flatten() {
        if prev.is_some_and(|p| d.dot(p) < 0.0) {
            *d = -*d;
        }
        prev = Some(*d);
    }

    let base = |i: usize| match (trace.kind, ss[i]) {
        (PolymerKind::Nucleic, _) => NUCLEIC,
        (_, Ss::Helix) => HELIX,
        (_, Ss::Sheet) => SHEET,
        _ => LOOP,
    };
    let is_arrow = |i: usize| ss[i] == Ss::Sheet && i + 1 < n && ss[i + 1] != Ss::Sheet;

    // Reflect past the ends so the spline keeps a sensible tangent at the first/last residue.
    let point = |i: isize| -> Vec3 {
        if i < 0 {
            points[0] * 2.0 - points[1]
        } else if i as usize >= n {
            points[n - 1] * 2.0 - points[n - 2]
        } else {
            points[i as usize]
        }
    };

    let samples = (0..n - 1)
        .flat_map(|i| (0..SAMPLES_PER_RESIDUE).map(move |k| (i, k as f32 / SAMPLES_PER_RESIDUE as f32)))
        .chain(std::iter::once((n - 2, 1.0)));

    let first_ring = mesh.vertices.len() as u32;
    let mut width_dir: Option<Vec3> = None;
    let mut rings = 0u32;
    let (mut first_center, mut last_center) = (None, None);

    for (i, t) in samples {
        let (p0, p1, p2, p3) = (point(i as isize - 1), point(i as isize), point(i as isize + 1), point(i as isize + 2));
        let (pos, tangent) = catmull_rom(p0, p1, p2, p3, t);
        let tangent = tangent.normalize_or(Vec3::X);

        // Interpolate the residue width directions, keep them perpendicular to the curve, and
        // fall back to transporting the previous frame where the file gives no orientation.
        let wanted = match (dirs[i], dirs[i + 1]) {
            (Some(a), Some(b)) => Some(a.lerp(b, t)),
            (a, b) => a.or(b),
        };
        let orthogonal = |d: Vec3| (d - tangent * d.dot(tangent)).try_normalize();
        let d = wanted
            .and_then(orthogonal)
            .or_else(|| width_dir.and_then(orthogonal))
            .unwrap_or_else(|| tangent.any_orthonormal_vector());
        width_dir = Some(d);
        let normal_dir = tangent.cross(d);

        let profile = if is_arrow(i) {
            Profile { width: ARROW_WIDTH, thickness: SHEET.thickness }.lerp(LOOP, t)
        } else {
            base(i).lerp(base(i + 1), smoothstep(t))
        };

        // Each half-segment takes the color (and pick id) of the nearer residue.
        let nearer = if t < 0.5 { i } else { i + 1 };
        let color = color(trace.residues[nearer]);
        let id = crate::atoms::pick_id(trace.guide_atoms[nearer] as usize);
        for r in 0..RING {
            let theta = TAU * r as f32 / RING as f32;
            let (sin, cos) = theta.sin_cos();
            let offset = d * (profile.width * cos) + normal_dir * (profile.thickness * sin);
            let normal = (d * (cos / profile.width) + normal_dir * (sin / profile.thickness)).normalize();
            mesh.vertices.push(MeshVertex { position: (pos + offset).into(), normal: normal.into(), color, id });
        }
        if rings > 0 {
            let a = first_ring + (rings - 1) * RING as u32;
            let b = a + RING as u32;
            for r in 0..RING as u32 {
                let r1 = (r + 1) % RING as u32;
                mesh.indices.extend_from_slice(&[a + r, b + r, a + r1, a + r1, b + r, b + r1]);
            }
        }
        first_center.get_or_insert((pos, -tangent, color, id));
        last_center = Some((pos, tangent, color, id));
        rings += 1;
    }

    // Close both ends so the tube does not look hollow.
    let last_ring = first_ring + (rings - 1) * RING as u32;
    for ((center, normal, color, id), ring) in [(first_center.unwrap(), first_ring), (last_center.unwrap(), last_ring)] {
        let c = mesh.vertices.len() as u32;
        mesh.vertices.push(MeshVertex { position: center.into(), normal: normal.into(), color, id });
        for r in 0..RING as u32 {
            mesh.indices.extend_from_slice(&[c, ring + r, ring + (r + 1) % RING as u32]);
        }
    }
}

/// Uniform Catmull-Rom position and derivative between `p1` and `p2`.
fn catmull_rom(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: f32) -> (Vec3, Vec3) {
    let (t2, t3) = (t * t, t * t * t);
    let a = p1 * 2.0;
    let b = p2 - p0;
    let c = p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3;
    let d = -p0 + p1 * 3.0 - p2 * 3.0 + p3;
    let pos = (a + b * t + c * t2 + d * t3) * 0.5;
    let deriv = (b + c * (2.0 * t) + d * (3.0 * t2)) * 0.5;
    (pos, deriv)
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catmull_rom_interpolates_endpoints() {
        let p = [Vec3::ZERO, Vec3::X, Vec3::new(2.0, 1.0, 0.0), Vec3::new(3.0, 1.0, 0.0)];
        assert!(catmull_rom(p[0], p[1], p[2], p[3], 0.0).0.distance(p[1]) < 1e-6);
        assert!(catmull_rom(p[0], p[1], p[2], p[3], 1.0).0.distance(p[2]) < 1e-6);
    }
}
