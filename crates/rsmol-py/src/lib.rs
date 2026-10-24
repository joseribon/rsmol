//! Python bindings: an rsmol session that the pure-Python `rsmol.cmd` module drives, headless or
//! with a native viewer window (`run_viewer`, used by the `rsmol` command).
//!
//! Commands go through the same command language as the app (`Session.do`), so scripts and the
//! command bar behave identically. A `Session` is a handle to shared state: the viewer window
//! reads it on its own thread and is woken whenever a command changes something, so scripts
//! running alongside the window update it live.

mod viewer;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use glam::{Quat, Vec3};
use pyo3::exceptions::{PyIOError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use rsmol_core::session::{AtomField, Effect, Session as CoreSession, Source};
use rsmol_core::session_file::{CameraState, EXTENSION};
use rsmol_render::Camera;
use rsmol_render::offscreen::Offscreen;

/// Aspect ratio used for the camera stored in saved sessions.
const SAVE_ASPECT: f32 = 1.6;

/// A change of view requested by a command, applied by the viewer (or by `png` when headless).
#[derive(Clone, Debug)]
enum ViewRequest {
    /// Frame everything.
    Fit,
    /// Frame these atoms.
    Focus(Vec<usize>),
    Turn(Vec3, f32),
    /// Default orientation, everything framed.
    Reset,
    /// An exact camera from a session file.
    Saved(CameraState),
}

struct State {
    core: CoreSession,
    /// Headless view: atoms to frame (`None` = everything), orientation, and a saved camera.
    framing: Option<Vec<usize>>,
    rotation: Quat,
    saved_camera: Option<CameraState>,
    /// While a viewer is open: its current camera, so `png` and `save` match the window.
    viewer_camera: Option<CameraState>,
    /// View changes for the viewer to apply, in order.
    view_requests: Vec<ViewRequest>,
    /// Bumped on every change; the viewer redraws when it moves.
    version: u64,
    offscreen: Option<Offscreen>,
}

/// State shared between Python and the viewer thread.
struct Shared {
    state: Mutex<State>,
    /// Wakes the viewer's event loop; set while a viewer runs.
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Asks the viewer to close.
    quit: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wake(&self) {
        if let Some(wake) = self.waker.lock().unwrap().as_ref() {
            wake();
        }
    }
}

#[pyclass(module = "rsmol._native")]
struct Session {
    shared: Arc<Shared>,
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

fn camera_state(c: &Camera) -> CameraState {
    CameraState { target: c.target.into(), distance: c.distance, rotation: c.rotation.into(), fov_y: c.fov_y }
}

/// Places `camera` exactly as `state` says, keeping the scene bounds it already has.
fn apply_camera_state(camera: &mut Camera, state: &CameraState) {
    camera.target = state.target.into();
    camera.distance = state.distance;
    camera.rotation = Quat::from_array(state.rotation).normalize();
    camera.fov_y = state.fov_y;
}

impl State {
    fn structure(&self) -> PyResult<&rsmol_core::Structure> {
        self.core.structure.as_deref().ok_or_else(|| PyRuntimeError::new_err("no structure loaded"))
    }

    /// Records a command's effects on the view, and marks the state changed.
    fn apply(&mut self, effects: &[Effect]) {
        for effect in effects {
            match effect {
                Effect::Focus(atoms) => {
                    self.framing = atoms.clone();
                    self.saved_camera = None;
                    self.view_requests.push(atoms.clone().map_or(ViewRequest::Fit, ViewRequest::Focus));
                }
                Effect::Scene { refit: true } => {
                    self.framing = None;
                    self.saved_camera = None;
                    self.view_requests.push(ViewRequest::Fit);
                }
                Effect::Highlight(_) | Effect::Scene { refit: false } | Effect::Display | Effect::Measurements => {}
            }
        }
        self.version += 1;
    }

    /// The camera for an image of `width`×`height`: the viewer's while one is open, else the
    /// headless view.
    fn camera(&self, width: u32, height: u32) -> Camera {
        let aspect = width as f32 / height.max(1) as f32;
        let mut camera = Camera::default();
        let Some(s) = self.core.structure.as_ref() else { return camera };
        let coords = s.coords(0).unwrap_or_default();
        camera.fit(coords, aspect);
        camera.rotation = self.rotation;
        if let Some(view) = self.viewer_camera.as_ref().or(self.saved_camera.as_ref()) {
            apply_camera_state(&mut camera, view);
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
            self.core = session;
            self.framing = None;
            self.saved_camera = camera;
            self.view_requests.push(camera.map_or(ViewRequest::Fit, ViewRequest::Saved));
            self.version += 1;
            return Ok(file_name);
        }
        let mut structure = rsmol_core::io::parse_bytes(&bytes, &file_name).map_err(|e| PyValueError::new_err(e.to_string()))?;
        if let Some(name) = object {
            structure.name = name.to_owned();
        }
        let (name, effects) = self.core.add_object(Arc::new(structure), Some(Source { file_name, data: Arc::new(bytes) }));
        self.apply(&effects);
        Ok(name)
    }
}

impl Session {
    /// Runs `f` on the state; if it changes anything (bumps the version), wakes the viewer
    /// after the lock is released.
    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let (result, changed) = {
            let mut state = self.shared.lock();
            let before = state.version;
            let result = f(&mut state);
            (result, state.version != before)
        };
        if changed {
            self.shared.wake();
        }
        result
    }
}

#[pymethods]
impl Session {
    #[new]
    fn new() -> Self {
        let state = State {
            core: CoreSession::default(),
            framing: None,
            rotation: default_rotation(),
            saved_camera: None,
            viewer_camera: None,
            view_requests: Vec::new(),
            version: 0,
            offscreen: None,
        };
        Session {
            shared: Arc::new(Shared {
                state: Mutex::new(state),
                waker: Mutex::new(None),
                quit: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// Loads a structure file as a new object (named `object`, or after the file), or replaces
    /// everything with a `.rsmol` session. Returns the object name.
    #[pyo3(signature = (path, object=None))]
    fn load(&self, path: PathBuf, object: Option<&str>) -> PyResult<String> {
        self.with(|s| s.add_file(&path, object))
    }

    /// Downloads a PDB entry from RCSB (cached on disk) and loads it. Returns the object name.
    #[pyo3(signature = (code, object=None))]
    fn fetch(&self, py: Python<'_>, code: &str, object: Option<&str>) -> PyResult<String> {
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
        self.with(|s| s.add_file(&path, object))
    }

    /// Runs a command line (the app's command language) and returns its message.
    fn r#do(&self, command: &str) -> PyResult<String> {
        self.with(|s| {
            let outcome = s.core.run(command, false).map_err(|e| value_error(e, command))?;
            s.apply(&outcome.effects);
            Ok(outcome.message)
        })
    }

    /// Like `do`, returning the message, atom count and any numbers the command produced.
    fn run<'py>(&self, py: Python<'py>, command: &str) -> PyResult<Bound<'py, PyDict>> {
        let outcome = self.with(|s| {
            let outcome = s.core.run(command, false).map_err(|e| value_error(e, command))?;
            s.apply(&outcome.effects);
            Ok::<_, PyErr>(outcome)
        })?;
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
        let state = self.shared.lock();
        Ok(state.core.select_atoms(selection).map_err(|e| value_error(e, selection))?.count())
    }

    /// Coordinates (Å) of the selected atoms, in atom order.
    #[pyo3(signature = (selection="all"))]
    fn get_coords(&self, selection: &str) -> PyResult<Vec<[f32; 3]>> {
        let state = self.shared.lock();
        let set = state.core.select_atoms(selection).map_err(|e| value_error(e, selection))?;
        let coords = state.structure()?.coords(0).unwrap_or_default();
        Ok(set.iter().map(|i| coords[i]).collect())
    }

    /// One dict per selected atom: index (1-based), object, chain, segi, resn, resi, name, elem,
    /// alt, b, q, x, y, z, hetatm.
    #[pyo3(signature = (selection="all"))]
    fn get_atoms<'py>(&self, py: Python<'py>, selection: &str) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let atoms = self.shared.lock().core.atom_info(selection).map_err(|e| value_error(e, selection))?;
        atoms
            .into_iter()
            .map(|a| {
                let d = PyDict::new(py);
                d.set_item("index", a.index)?;
                d.set_item("object", a.object)?;
                d.set_item("chain", a.chain)?;
                d.set_item("segi", a.segi)?;
                d.set_item("resn", a.resn)?;
                d.set_item("resi", a.resi)?;
                d.set_item("name", a.name)?;
                d.set_item("elem", a.elem)?;
                d.set_item("alt", a.alt)?;
                d.set_item("b", a.b)?;
                d.set_item("q", a.q)?;
                d.set_item("x", a.x)?;
                d.set_item("y", a.y)?;
                d.set_item("z", a.z)?;
                d.set_item("hetatm", a.hetatm)?;
                Ok(d)
            })
            .collect()
    }

    /// Object names in load order.
    fn get_names(&self) -> Vec<String> {
        self.shared.lock().core.objects.iter().map(|o| o.name.clone()).collect()
    }

    /// Measurements as dicts: name, kind, value (Å or degrees), atoms (1-based indices).
    fn get_measurements<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let measurements = self.shared.lock().core.measurement_info();
        measurements
            .into_iter()
            .map(|m| {
                let d = PyDict::new(py);
                d.set_item("name", m.name)?;
                d.set_item("kind", m.kind)?;
                d.set_item("value", m.value)?;
                d.set_item("atoms", m.atoms)?;
                Ok(d)
            })
            .collect()
    }

    /// Rotates the view about a screen axis ("x", "y" or "z") by `angle` degrees.
    fn turn(&self, axis: &str, angle: f32) -> PyResult<()> {
        let axis = match axis.to_ascii_lowercase().as_str() {
            "x" => Vec3::X,
            "y" => Vec3::Y,
            "z" => Vec3::Z,
            _ => return Err(PyValueError::new_err("axis must be 'x', 'y' or 'z'")),
        };
        let q = Quat::from_axis_angle(axis, angle.to_radians());
        self.with(|s| {
            match &mut s.saved_camera {
                Some(saved) => saved.rotation = (Quat::from_array(saved.rotation) * q).normalize().to_array(),
                None => s.rotation = (s.rotation * q).normalize(),
            }
            s.view_requests.push(ViewRequest::Turn(axis, angle));
            s.version += 1;
        });
        Ok(())
    }

    /// Closes the viewer window if one is open; returns whether one was.
    fn quit(&self) -> bool {
        let open = self.shared.waker.lock().unwrap().is_some();
        if open {
            self.shared.quit.store(true, std::sync::atomic::Ordering::SeqCst);
            self.shared.wake();
        }
        open
    }

    /// Back to the default view of everything.
    fn reset(&self) {
        self.with(|s| {
            s.rotation = default_rotation();
            s.framing = None;
            s.saved_camera = None;
            s.view_requests.push(ViewRequest::Reset);
            s.version += 1;
        });
    }

    /// Renders the current view (the window's, while one is open) to a PNG file on the GPU, with
    /// ambient occlusion and outlines. Selection markers are interactive aids and, as in PyMOL,
    /// are left out of figures.
    #[pyo3(signature = (path, width=1600, height=1000))]
    fn png(&self, py: Python<'_>, path: PathBuf, width: u32, height: u32) -> PyResult<()> {
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err(PyValueError::new_err("width and height must be between 1 and 16384"));
        }
        // Gather what the render needs, then release the lock (and the GIL) while it runs.
        let (structure, display, segments, camera, mut offscreen) = {
            let mut state = self.shared.lock();
            let structure = state.core.structure.clone().ok_or_else(|| PyRuntimeError::new_err("no structure loaded"))?;
            let display = state.core.render_display().ok_or_else(|| PyRuntimeError::new_err("no structure loaded"))?;
            let coords = structure.coords(0).unwrap_or_default();
            let segments: Vec<_> = state.core.measurements.iter().flat_map(|m| m.segments(coords)).collect();
            (structure.clone(), display, segments, state.camera(width, height), state.offscreen.take())
        };
        let pixels = py.detach(|| -> Result<Vec<u8>, String> {
            if offscreen.is_none() {
                offscreen = Some(Offscreen::new(width, height)?);
            }
            let r = offscreen.as_mut().unwrap();
            r.resize(width, height);
            r.set_scene(&structure, &display, &[], &segments);
            Ok(r.render(&camera))
        });
        self.shared.lock().offscreen = offscreen;
        let pixels = pixels.map_err(PyRuntimeError::new_err)?;
        let file = std::fs::File::create(&path).map_err(|e| PyIOError::new_err(format!("{}: {e}", path.display())))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| PyIOError::new_err(e.to_string()))?;
        writer.write_image_data(&pixels).map_err(|e| PyIOError::new_err(e.to_string()))
    }

    /// Sets atom properties: `(index, field, value)` triples with 1-based indices and the fields
    /// name, resn, resi, chain, segi, elem, alt, b, q. One undo step. Returns the change count.
    fn alter(&self, changes: Vec<(usize, String, String)>) -> PyResult<usize> {
        let changes = changes
            .into_iter()
            .map(|(index, field, value)| {
                let field = AtomField::from_name(&field)
                    .ok_or_else(|| PyValueError::new_err(format!("cannot alter '{field}'")))?;
                let atom = index.checked_sub(1).ok_or_else(|| PyValueError::new_err("atom indices start at 1"))?;
                Ok((atom, field, value))
            })
            .collect::<PyResult<Vec<_>>>()?;
        let count = changes.len();
        self.with(|s| {
            let effects = s.core.alter(&changes).map_err(PyValueError::new_err)?;
            s.apply(&effects);
            Ok(count)
        })
    }

    /// Writes the selected atoms as PDB or mmCIF, chosen by the file extension.
    #[pyo3(signature = (path, selection="all"))]
    fn export(&self, path: PathBuf, selection: &str) -> PyResult<()> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
        let text = self.shared.lock().core.export(ext, selection).map_err(PyValueError::new_err)?;
        std::fs::write(&path, text).map_err(|e| PyIOError::new_err(format!("{}: {e}", path.display())))
    }

    /// Saves a `.rsmol` session (opens in the rsmol app) with the current view.
    fn save(&self, path: PathBuf) -> PyResult<()> {
        let bytes = {
            let state = self.shared.lock();
            let camera = camera_state(&state.camera((SAVE_ASPECT * 1000.0) as u32, 1000));
            state.core.save(Some(camera)).map_err(PyRuntimeError::new_err)?
        };
        std::fs::write(&path, bytes).map_err(|e| PyIOError::new_err(format!("{}: {e}", path.display())))
    }
}

/// Opens a viewer window on `session` and runs it until the window closes. Must be called from
/// the main thread; other Python threads keep running and their commands update the window.
#[pyfunction]
#[pyo3(signature = (session, title="rsmol"))]
fn run_viewer(py: Python<'_>, session: &Session, title: &str) -> PyResult<()> {
    let shared = session.shared.clone();
    let title = title.to_owned();
    py.detach(move || viewer::run(shared, &title)).map_err(PyRuntimeError::new_err)
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Session>()?;
    m.add_function(wrap_pyfunction!(run_viewer, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
