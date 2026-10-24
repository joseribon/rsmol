//! Geometric measurements between atoms: distances, angles and dihedrals.

use crate::Vec3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasurementKind {
    Distance,
    Angle,
    Dihedral,
}

impl MeasurementKind {
    /// Atoms the measurement takes.
    pub fn arity(self) -> usize {
        match self {
            MeasurementKind::Distance => 2,
            MeasurementKind::Angle => 3,
            MeasurementKind::Dihedral => 4,
        }
    }

    /// Prefix for automatic names: dist01, angle01, dihe01 (as PyMOL names them).
    pub fn prefix(self) -> &'static str {
        match self {
            MeasurementKind::Distance => "dist",
            MeasurementKind::Angle => "angle",
            MeasurementKind::Dihedral => "dihe",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    pub name: String,
    pub kind: MeasurementKind,
    pub atoms: Vec<usize>,
    /// Å for distances, degrees for angles and dihedrals.
    pub value: f32,
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(a: Vec3) -> f32 {
    dot(a, a).sqrt()
}

fn lerp(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

impl Measurement {
    /// Measures `atoms` (as many as `kind` takes) in `coords`.
    pub fn new(name: String, kind: MeasurementKind, atoms: Vec<usize>, coords: &[Vec3]) -> Measurement {
        assert_eq!(atoms.len(), kind.arity());
        let p: Vec<Vec3> = atoms.iter().map(|&i| coords[i]).collect();
        let value = match kind {
            MeasurementKind::Distance => norm(sub(p[1], p[0])),
            MeasurementKind::Angle => {
                let (u, v) = (sub(p[0], p[1]), sub(p[2], p[1]));
                (dot(u, v) / (norm(u) * norm(v)).max(1e-12)).clamp(-1.0, 1.0).acos().to_degrees()
            }
            MeasurementKind::Dihedral => {
                // Signed torsion about the p1–p2 bond (IUPAC convention).
                let (b0, b1, b2) = (sub(p[0], p[1]), sub(p[2], p[1]), sub(p[3], p[2]));
                let b1n = norm(b1).max(1e-12);
                let u = [b1[0] / b1n, b1[1] / b1n, b1[2] / b1n];
                let v = sub(b0, [u[0] * dot(b0, u), u[1] * dot(b0, u), u[2] * dot(b0, u)]);
                let w = sub(b2, [u[0] * dot(b2, u), u[1] * dot(b2, u), u[2] * dot(b2, u)]);
                dot(cross(u, v), w).atan2(dot(v, w)).to_degrees()
            }
        };
        Measurement { name, kind, atoms, value }
    }

    pub fn label(&self) -> String {
        match self.kind {
            MeasurementKind::Distance => format!("{:.2} Å", self.value),
            _ => format!("{:.1}°", self.value),
        }
    }

    /// Line segments to draw: the bond(s) the measurement spans.
    pub fn segments(&self, coords: &[Vec3]) -> Vec<(Vec3, Vec3)> {
        let p: Vec<Vec3> = self.atoms.iter().map(|&i| coords[i]).collect();
        p.windows(2).map(|w| (w[0], w[1])).collect()
    }

    /// Where to place the label: the segment midpoint for distances, near the vertex for angles
    /// and the central bond for dihedrals.
    pub fn anchor(&self, coords: &[Vec3]) -> Vec3 {
        let p: Vec<Vec3> = self.atoms.iter().map(|&i| coords[i]).collect();
        match self.kind {
            MeasurementKind::Distance => lerp(p[0], p[1], 0.5),
            // A third of the way into the angle, between the two arms.
            MeasurementKind::Angle => lerp(p[1], lerp(p[0], p[2], 0.5), 0.35),
            MeasurementKind::Dihedral => lerp(p[1], p[2], 0.5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        let coords = [[0.0, 0.0, 0.0], [1.5, 0.0, 0.0], [1.5, 1.5, 0.0], [1.5, 1.5, 1.5]];
        let m = |kind, atoms: &[usize]| Measurement::new("m".into(), kind, atoms.to_vec(), &coords);
        assert!((m(MeasurementKind::Distance, &[0, 1]).value - 1.5).abs() < 1e-6);
        assert!((m(MeasurementKind::Angle, &[0, 1, 2]).value - 90.0).abs() < 1e-4);
        assert!((m(MeasurementKind::Dihedral, &[0, 1, 2, 3]).value.abs() - 90.0).abs() < 1e-4);
        assert_eq!(m(MeasurementKind::Distance, &[0, 1]).label(), "1.50 Å");
        assert_eq!(m(MeasurementKind::Angle, &[0, 1, 2]).label(), "90.0°");
    }

    #[test]
    fn dihedral_sign_follows_iupac() {
        // Looking down the 1→2 bond, a clockwise turn from atom 0 to atom 3 is positive.
        let coords = [[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 1.0]];
        let d = Measurement::new("d".into(), MeasurementKind::Dihedral, vec![0, 1, 2, 3], &coords);
        assert!((d.value - 90.0).abs() < 1e-4, "{}", d.value);
    }
}
