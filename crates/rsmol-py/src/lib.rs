//! Python bindings: a headless rsmol session that the pure-Python `rsmol.cmd` module drives.
//!
//! Commands go through the same command language as the app (`Session.do`), so scripts and the
//! command bar behave identically. On top of that the session keeps a camera for `png`, and
//! exposes structured data (atoms, coordinates, measurements) for analysis.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use glam::{Quat, Vec3};
use pyo3::exceptions::{PyIOError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use rsmol_core::session::{Effect, Session as CoreSession, Source};
use rsmol_core::session_file::{CameraState, EXTENSION};
use rsmol_render::Camera;
use rsmol_render::offscreen::Offscreen;

/// Aspect ratio used for the camera stored in saved sessions.
const SAVE_ASPECT: f32 = 1.6;

#[pyclass(module = "rsmol._native")]
struct Session {
    inner: CoreSession,
    /// Atoms the view is framed on; `None` frames everything.
    framing: Option<Vec<usize>>,
    rotation: Quat,
    /// Exact camera restored from a session file, until the next zoom.
    saved_camera: Option<CameraState>,
    offscreen: Option<Offscreen>,
}

fn default_rotation() -> Quat {
    // The slight tilt the viewer and app use, which reads better than the file orientation.
    let mut c = Camera::default();
    c.orbit(-120.0, -60.0);
    c.rotation
}

fn value_error(e: rsmol_core::select::SelectionError, text: &str) -> PyErr {
    PyValueError::new_err(e.render(text))
}

fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("rsmol");
    }
    if cfg!(target_os = "macos") { home.join("Library/Caches/rsmol") } else { home.join(".cache/rsmol") }
}

impl Session {
    fn structure(&self) -> PyResult<&rsmol_core::Structure> {
        self.inner.structure.as_deref().ok_or_else(|| PyRuntimeError::new_err("no structure loaded"))
    }

    fn apply(&mut self, effects: &[Effect]) {
        for effect in effects {
            match effect {
                Effect::Focus(atoms) => {
                    self.framing = atoms.clone();
                    self.saved_camera = None;
                }
                Effect::Scene { refit: true } => {
                    self.framing = None;
                    self.saved_camera = None;
                }
                Effect::Highlight(_) | Effect::Scene { refit: false } | Effect::Display | Effect::Measurements => {}
            }
        }
    }

    fn camera(&self, width: u32, height: u32) -> Camera {
        let aspect = width as f32 / height.max(1) as f32;
        let mut camera = Camera::default();
        let Some(s) = self.inner.structure.as_ref() else { return camera };
        let coords = s.coords(0).unwrap_or_default();
        camera.fit(coords, aspect);
        camera.rotation = self.rotation;
        if let Some(saved) = self.saved_camera {
            camera.target = saved.target.into();
            camera.distance = saved.distance;
            camera.rotation = Quat::from_array(saved.rotation).normalize();
            camera.fov_y = saved.fov_y;
        } else if let Some(atoms) = &self.framing {
            let points: Vec<_> = atoms.iter().map(|&i| coords[i]).collect();
            camera.focus(&points, aspect);
        }
        camera
    }

    fn add_file(&mut self, path: &Path, object: Option<&str>) -> PyResult<String> {
        let bytes = std::fs::read(path).map_err(|e| PyIOError::new_err(format!("{}: {e}", path.display())))?;
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION)) {
            let (session, camera) = CoreSession::open(&bytes).map_err(PyValueError::new_err)?;
            self.inner = session;
            self.framing = None;
            self.saved_camera = camera;
            return Ok(file_name);
        }
        let mut structure = rsmol_core::io::parse_bytes(&bytes, &file_name).map_err(|e| PyValueError::new_err(e.to_string()))?;
        if let Some(name) = object {
            structure.name = name.to_owned();
        }
        let (name, effects) = self.inner.add_object(Arc::new(structure), Some(Source { file_name, data: Arc::new(bytes) }));
        self.apply(&effects);
        Ok(name)
    }
}

#[pymethods]
impl Session {
    #[new]
    fn new() -> Self {
        Session {
            inner: CoreSession::default(),
            framing: None,
            rotation: default_rotation(),
            saved_camera: None,
            offscreen: None,
        }
    }

    /// Loads a structure file as a new object (named `object`, or after the file), or replaces
    /// everything with a `.rsmol` session. Returns the object name.
    #[pyo3(signature = (path, object=None))]
    fn load(&mut self, path: PathBuf, object: Option<&str>) -> PyResult<String> {
        self.add_file(&path, object)
    }

    /// Downloads a PDB entry from RCSB (cached on disk) and loads it. Returns the object name.
    #[pyo3(signature = (code, object=None))]
    fn fetch(&mut self, py: Python<'_>, code: &str, object: Option<&str>) -> PyResult<String> {
        let id = code.trim().to_ascii_uppercase();
        let valid = id.len() == 4 && id.starts_with(|c: char| c.is_ascii_digit()) && id.chars().all(|c| c.is_ascii_alphanumeric());
        if !valid {
            return Err(PyValueError::new_err(format!("'{code}' is not a PDB ID (e.g. 1HSG)")));
        }
        let path = cache_dir().join("pdb").join(format!("{id}.cif.gz"));
        if !path.exists() {
            let url = format!("https://files.rcsb.org/download/{id}.cif.gz");
            // Release the GIL while downloading so other Python threads keep running.
            let bytes = py
                .detach(|| -> Result<Vec<u8>, String> {
                    let mut response = ureq::get(&url).call().map_err(|e| match e {
                        ureq::Error::StatusCode(404) => format!("{id} was not found at the RCSB PDB"),
                        e => format!("download failed: {e}"),
                    })?;
                    response.body_mut().with_config().limit(512 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())
                })
                .map_err(PyIOError::new_err)?;
            let dir = path.parent().unwrap();
            std::fs::create_dir_all(dir).map_err(|e| PyIOError::new_err(e.to_string()))?;
            let partial = path.with_extension("partial");
            std::fs::write(&partial, bytes).and_then(|_| std::fs::rename(&partial, &path)).map_err(|e| PyIOError::new_err(e.to_string()))?;
        }
        self.add_file(&path, object)
    }

    /// Runs a command line (the app's command language) and returns its message.
    fn r#do(&mut self, command: &str) -> PyResult<String> {
        let outcome = self.inner.run(command, false).map_err(|e| value_error(e, command))?;
        self.apply(&outcome.effects);
        Ok(outcome.message)
    }

    /// Like `do`, returning the message, atom count and any numbers the command produced.
    fn run<'py>(&mut self, py: Python<'py>, command: &str) -> PyResult<Bound<'py, PyDict>> {
        let outcome = self.inner.run(command, false).map_err(|e| value_error(e, command))?;
        self.apply(&outcome.effects);
        let d = PyDict::new(py);
        d.set_item("message", outcome.message)?;
        d.set_item("atoms", outcome.atoms)?;
        for (key, value) in outcome.values {
            d.set_item(key, value)?;
        }
        Ok(d)
    }

    #[pyo3(signature = (selection="all"))]
    fn count_atoms(&self, selection: &str) -> PyResult<usize> {
        Ok(self.inner.select_atoms(selection).map_err(|e| value_error(e, selection))?.count())
    }

    /// Coordinates (Å) of the selected atoms, in atom order.
    #[pyo3(signature = (selection="all"))]
    fn get_coords(&self, selection: &str) -> PyResult<Vec<[f32; 3]>> {
        let set = self.inner.select_atoms(selection).map_err(|e| value_error(e, selection))?;
        let coords = self.structure()?.coords(0).unwrap_or_default();
        Ok(set.iter().map(|i| coords[i]).collect())
    }

    /// One dict per selected atom: index (1-based), object, chain, segi, resn, resi, name, elem,
    /// alt, b, q, x, y, z, hetatm.
    #[pyo3(signature = (selection="all"))]
    fn get_atoms<'py>(&self, py: Python<'py>, selection: &str) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let set = self.inner.select_atoms(selection).map_err(|e| value_error(e, selection))?;
        let s = self.structure()?;
        let coords = s.coords(0).unwrap_or_default();
        let objects = self.inner.object_ranges();
        set.iter()
            .map(|i| {
                let r = s.residue_of(i);
                let d = PyDict::new(py);
                d.set_item("index", i + 1)?;
                let object = objects.iter().find(|(_, range)| range.contains(&i)).map(|(n, _)| n.as_str());
                d.set_item("object", object)?;
                d.set_item("chain", &s.chain_of(i).id)?;
                d.set_item("segi", &s.chain_of(i).label_id)?;
                d.set_item("resn", &r.name)?;
                d.set_item("resi", format!("{}{}", r.seq_num, r.ins_code.map(String::from).unwrap_or_default()))?;
                d.set_item("name", &s.atoms.name[i])?;
                d.set_item("elem", s.atoms.element[i].symbol())?;
                d.set_item("alt", s.atoms.alt_loc[i].map(String::from).unwrap_or_default())?;
                d.set_item("b", s.atoms.b_factor[i])?;
                d.set_item("q", s.atoms.occupancy[i])?;
                let [x, y, z] = coords[i];
                d.set_item("x", x)?;
                d.set_item("y", y)?;
                d.set_item("z", z)?;
                d.set_item("hetatm", s.atoms.hetero[i])?;
                Ok(d)
            })
            .collect()
    }

    /// Object names in load order.
    fn get_names(&self) -> Vec<String> {
        self.inner.objects.iter().map(|o| o.name.clone()).collect()
    }

    /// Measurements as dicts: name, kind, value (Å or degrees), atoms (1-based indices).
    fn get_measurements<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        self.inner
            .measurements
            .iter()
            .map(|m| {
                let d = PyDict::new(py);
                d.set_item("name", &m.name)?;
                d.set_item("kind", format!("{:?}", m.kind).to_lowercase())?;
                d.set_item("value", m.value)?;
                d.set_item("atoms", m.atoms.iter().map(|a| a + 1).collect::<Vec<_>>())?;
                Ok(d)
            })
            .collect()
    }

    /// Rotates the view about a screen axis ("x", "y" or "z") by `angle` degrees.
    fn turn(&mut self, axis: &str, angle: f32) -> PyResult<()> {
        let axis = match axis.to_ascii_lowercase().as_str() {
            "x" => Vec3::X,
            "y" => Vec3::Y,
            "z" => Vec3::Z,
            _ => return Err(PyValueError::new_err("axis must be 'x', 'y' or 'z'")),
        };
        let q = Quat::from_axis_angle(axis, angle.to_radians());
        match &mut self.saved_camera {
            Some(saved) => saved.rotation = (Quat::from_array(saved.rotation) * q).normalize().to_array(),
            None => self.rotation = (self.rotation * q).normalize(),
        }
        Ok(())
    }

    /// Back to the default view of everything.
    fn reset(&mut self) {
        self.rotation = default_rotation();
        self.framing = None;
        self.saved_camera = None;
    }

    /// Renders the current view to a PNG file on the GPU, with ambient occlusion and outlines.
    /// Selection markers are interactive aids and, as in PyMOL, are left out of figures.
    #[pyo3(signature = (path, width=1600, height=1000))]
    fn png(&mut self, py: Python<'_>, path: PathBuf, width: u32, height: u32) -> PyResult<()> {
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err(PyValueError::new_err("width and height must be between 1 and 16384"));
        }
        let structure = self.inner.structure.clone().ok_or_else(|| PyRuntimeError::new_err("no structure loaded"))?;
        let display = self.inner.render_display().ok_or_else(|| PyRuntimeError::new_err("no structure loaded"))?;
        let coords = structure.coords(0).unwrap_or_default();
        let segments: Vec<_> = self.inner.measurements.iter().flat_map(|m| m.segments(coords)).collect();
        let camera = self.camera(width, height);
        let offscreen = &mut self.offscreen;
        let pixels = py.detach(|| -> Result<Vec<u8>, String> {
            if offscreen.is_none() {
                *offscreen = Some(Offscreen::new(width, height)?);
            }
            let r = offscreen.as_mut().unwrap();
            r.resize(width, height);
            r.set_scene(&structure, &display, &[], &segments);
            Ok(r.render(&camera))
        });
        let pixels = pixels.map_err(PyRuntimeError::new_err)?;
        let file = std::fs::File::create(&path).map_err(|e| PyIOError::new_err(format!("{}: {e}", path.display())))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| PyIOError::new_err(e.to_string()))?;
        writer.write_image_data(&pixels).map_err(|e| PyIOError::new_err(e.to_string()))
    }

    /// Saves a `.rsmol` session (opens in the rsmol app) with the current view.
    fn save(&self, path: PathBuf) -> PyResult<()> {
        let c = self.camera((SAVE_ASPECT * 1000.0) as u32, 1000);
        let camera = CameraState { target: c.target.into(), distance: c.distance, rotation: c.rotation.into(), fov_y: c.fov_y };
        let bytes = self.inner.save(Some(camera)).map_err(PyRuntimeError::new_err)?;
        std::fs::write(&path, bytes).map_err(|e| PyIOError::new_err(format!("{}: {e}", path.display())))
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Session>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
