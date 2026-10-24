//! `.rsmol` session files: a self-contained snapshot of a session.
//!
//! The file is gzip-compressed JSON holding each object's original structure file (so a session
//! opens anywhere, without the source files or a network) with the transform applied to it
//! (e.g. by `align`), the per-atom display state of the merged scene as base64 blobs, named
//! selections as index ranges, measurements, and the camera.
//!
//! Version 1 files held a single `source` instead of `objects`; they still open.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use crate::display::{Display, Reps};
use crate::measure::{Measurement, MeasurementKind};
use crate::select::AtomSet;
use crate::session::{Object, Session, Source};
use crate::superpose::Transform;

pub const EXTENSION: &str = "rsmol";
const FORMAT: &str = "rsmol-session";
const VERSION: u32 = 2;

/// Camera placement, stored as plain data so the core stays free of renderer types.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraState {
    pub target: [f32; 3],
    pub distance: f32,
    /// Unit quaternion (x, y, z, w).
    pub rotation: [f32; 4],
    pub fov_y: f32,
}

#[derive(Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    /// Version 2+.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    objects: Vec<ObjectData>,
    /// Version 1 only: the single structure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<SourceData>,
    display: DisplayData,
    /// Name → inclusive atom index ranges.
    selections: Vec<(String, Vec<[u32; 2]>)>,
    measurements: Vec<MeasurementData>,
    camera: Option<CameraState>,
}

#[derive(Serialize, Deserialize)]
struct ObjectData {
    name: String,
    #[serde(flatten)]
    source: SourceData,
    enabled: bool,
    rotation: [[f32; 3]; 3],
    translation: [f32; 3],
}

#[derive(Serialize, Deserialize)]
struct SourceData {
    file_name: String,
    /// Base64 of the original file, exactly as read (gzipped files stay gzipped).
    data: String,
}

#[derive(Serialize, Deserialize)]
struct DisplayData {
    atoms: usize,
    /// Base64 of one representation-flag byte per atom.
    reps: String,
    /// Base64 of RGBA bytes per atom.
    colors: String,
}

#[derive(Serialize, Deserialize)]
struct MeasurementData {
    name: String,
    kind: String,
    atoms: Vec<usize>,
}

fn kind_name(kind: MeasurementKind) -> &'static str {
    match kind {
        MeasurementKind::Distance => "distance",
        MeasurementKind::Angle => "angle",
        MeasurementKind::Dihedral => "dihedral",
    }
}

fn ranges(set: &AtomSet) -> Vec<[u32; 2]> {
    let mut out: Vec<[u32; 2]> = Vec::new();
    for i in set.iter().map(|i| i as u32) {
        match out.last_mut() {
            Some(r) if r[1] + 1 == i => r[1] = i,
            _ => out.push([i, i]),
        }
    }
    out
}

impl Session {
    /// Serializes the session. Fails if nothing is loaded, or an object has no source file.
    pub fn save(&self, camera: Option<CameraState>) -> Result<Vec<u8>, String> {
        let display = self.display.as_ref().ok_or("nothing to save: no structure is loaded")?;
        let mut objects = Vec::with_capacity(self.objects.len());
        for o in &self.objects {
            let source = o.source.as_ref().ok_or_else(|| format!("object {} has no source file to embed", o.name))?;
            objects.push(ObjectData {
                name: o.name.clone(),
                source: SourceData { file_name: source.file_name.clone(), data: BASE64.encode(source.data.as_slice()) },
                enabled: o.enabled,
                rotation: o.transform.rotation,
                translation: o.transform.translation,
            });
        }
        let reps: Vec<u8> = display.reps.iter().map(|r| r.bits()).collect();
        let colors: Vec<u8> = display.colors.iter().flatten().copied().collect();
        let mut selections: Vec<_> = self.named.iter().map(|(name, set)| (name.clone(), ranges(set))).collect();
        selections.sort_by(|a, b| a.0.cmp(&b.0));
        let file = File {
            format: FORMAT.to_owned(),
            version: VERSION,
            objects,
            source: None,
            display: DisplayData { atoms: display.reps.len(), reps: BASE64.encode(reps), colors: BASE64.encode(colors) },
            selections,
            measurements: self
                .measurements
                .iter()
                .map(|m| MeasurementData { name: m.name.clone(), kind: kind_name(m.kind).to_owned(), atoms: m.atoms.clone() })
                .collect(),
            camera,
        };
        let json = serde_json::to_vec(&file).map_err(|e| e.to_string())?;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&json).map_err(|e| e.to_string())?;
        gz.finish().map_err(|e| e.to_string())
    }

    /// Restores a session saved with [`Session::save`].
    pub fn open(bytes: &[u8]) -> Result<(Session, Option<CameraState>), String> {
        let mut json = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut json)
            .map_err(|_| "not an rsmol session file")?;
        let file: File = serde_json::from_slice(&json).map_err(|e| format!("damaged session file: {e}"))?;
        if file.format != FORMAT {
            return Err("not an rsmol session file".into());
        }
        if file.version > VERSION {
            return Err(format!("session file version {} is newer than this app supports ({VERSION})", file.version));
        }
        let mut object_data = file.objects;
        if let Some(source) = file.source {
            object_data.push(ObjectData {
                name: String::new(),
                source,
                enabled: true,
                rotation: Transform::IDENTITY.rotation,
                translation: Transform::IDENTITY.translation,
            });
        }
        if object_data.is_empty() {
            return Err("session file holds no structures".into());
        }

        let mut session = Session::default();
        for o in object_data {
            let data = BASE64.decode(&o.source.data).map_err(|e| format!("damaged structure data: {e}"))?;
            let parsed = crate::io::parse_bytes(&data, &o.source.file_name).map_err(|e| e.to_string())?;
            let transform = Transform { rotation: o.rotation, translation: o.translation };
            let structure = if transform == Transform::IDENTITY { parsed } else { transform_structure(parsed, &transform) };
            session.objects.push(Object {
                name: if o.name.is_empty() { structure.name.clone() } else { o.name },
                structure: Arc::new(structure),
                source: Some(Source { file_name: o.source.file_name, data: Arc::new(data) }),
                transform,
                enabled: o.enabled,
            });
        }
        session.rebuild_scene();
        let scene = session.structure.clone().expect("objects were loaded");
        let n = scene.atom_count();

        let reps = BASE64.decode(&file.display.reps).map_err(|e| e.to_string())?;
        let colors = BASE64.decode(&file.display.colors).map_err(|e| e.to_string())?;
        if file.display.atoms != n || reps.len() != n || colors.len() != n * 4 {
            return Err(format!("session display covers {} atoms but the structures have {n}", file.display.atoms));
        }
        session.display = Some(Display {
            reps: reps.into_iter().map(Reps::from_bits).collect(),
            colors: colors.chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect(),
        });

        let in_range = |i: usize| (i < n).then_some(i).ok_or_else(|| format!("atom index {i} out of range"));
        let mut named = HashMap::new();
        for (name, ranges) in file.selections {
            let mut set = AtomSet::empty(n);
            for [lo, hi] in ranges {
                in_range(hi as usize)?;
                for i in lo..=hi {
                    set.insert(i as usize);
                }
            }
            named.insert(name, set);
        }
        session.named = named;
        let coords = scene.coords(0).unwrap_or_default();
        for m in file.measurements {
            let kind = match m.kind.as_str() {
                "distance" => MeasurementKind::Distance,
                "angle" => MeasurementKind::Angle,
                "dihedral" => MeasurementKind::Dihedral,
                other => return Err(format!("unknown measurement kind '{other}'")),
            };
            if m.atoms.len() != kind.arity() {
                return Err(format!("measurement {} has {} atoms", m.name, m.atoms.len()));
            }
            for &a in &m.atoms {
                in_range(a)?;
            }
            session.measurements.push(Measurement::new(m.name, kind, m.atoms, coords));
        }
        Ok((session, file.camera))
    }
}

fn transform_structure(mut s: crate::Structure, t: &Transform) -> crate::Structure {
    for state in &mut s.states {
        for p in state.iter_mut() {
            *p = t.apply(*p);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/traces.pdb")).unwrap();
        let structure = crate::io::parse_bytes(&data, "traces.pdb").unwrap();
        let mut s = Session::default();
        s.load_with_source(Arc::new(structure), Source { file_name: "traces.pdb".into(), data: Arc::new(data) });
        s.run("color coral, chain A; show sticks, resi 1-3; select site, resi 2+5; distance index 2, index 6", false)
            .unwrap();
        let camera = CameraState { target: [1.0, 2.0, 3.0], distance: 40.0, rotation: [0.0, 0.0, 0.0, 1.0], fov_y: 0.5 };

        let bytes = s.save(Some(camera)).unwrap();
        let (restored, cam) = Session::open(&bytes).unwrap();
        assert_eq!(cam, Some(camera));
        assert_eq!(restored.display, s.display);
        assert_eq!(restored.named, s.named);
        assert_eq!(restored.measurements, s.measurements);
        assert_eq!(restored.structure.as_ref().unwrap().atom_count(), 27);
        // Undo history is not saved: a reopened session starts fresh.
        assert!(!restored.can_undo());
    }

    #[test]
    fn objects_and_transforms_round_trip() {
        let read = |f: &str| std::fs::read(format!("{}/tests/data/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let (a, b) = (read("traces.pdb"), read("two_models.cif"));
        let mut s = Session::default();
        let source = |f: &str, d: &Vec<u8>| Source { file_name: f.into(), data: Arc::new(d.clone()) };
        s.add_object(Arc::new(crate::io::parse_bytes(&a, "traces.pdb").unwrap()), Some(source("traces.pdb", &a)));
        s.add_object(Arc::new(crate::io::parse_bytes(&b, "two_models.cif").unwrap()), Some(source("two_models.cif", &b)));
        // Move the second object so the transform has to be stored.
        let shift = Transform { translation: [10.0, 0.0, 0.0], ..Transform::IDENTITY };
        let moved = transform_structure((*s.objects[1].structure).clone(), &shift);
        s.objects[1].structure = Arc::new(moved);
        s.objects[1].transform = shift;
        s.objects[1].enabled = false;
        s.rebuild_scene();

        let (restored, _) = Session::open(&s.save(None).unwrap()).unwrap();
        assert_eq!(restored.objects.len(), 2);
        assert_eq!(restored.objects[1].transform, shift);
        assert!(!restored.objects[1].enabled);
        assert_eq!(restored.structure.unwrap().states, s.structure.unwrap().states);
    }

    #[test]
    fn rejects_other_files() {
        assert_eq!(Session::open(b"hello").err().unwrap(), "not an rsmol session file");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(br#"{"format":"something-else"}"#).unwrap();
        assert!(Session::open(&gz.finish().unwrap()).err().unwrap().starts_with("damaged session file"));
        assert!(Session::default().save(None).is_err());
    }

    #[test]
    fn index_ranges() {
        let set = AtomSet::from_indices(20, [1, 2, 3, 7, 9, 10]);
        assert_eq!(ranges(&set), [[1, 3], [7, 7], [9, 10]]);
    }
}
