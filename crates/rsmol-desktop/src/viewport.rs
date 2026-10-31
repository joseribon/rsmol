//! The molecule, drawn natively by rsmol-render into a wgpui surface.
//!
//! All GPU state lives on a dedicated render thread fed by a channel. Each wakeup drains every
//! pending message before drawing one frame, so a burst of mouse events costs a single redraw
//! and the UI thread never waits on the GPU. The frame is rendered into the surface's back
//! buffer and handed to wgpui, which composites it under the UI.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use rsmol_core::Structure;
use rsmol_core::display::Display;
use rsmol_core::measure::Measurement;
use rsmol_core::session_file::CameraState;
use rsmol_render::{Camera, Renderer};
use wgpui::WgpuSurfaceHandle;

/// The surface format: wgpui composites the texture's values as they are, so the renderer
/// writes sRGB-encoded colors into a plain format (an sRGB format would be decoded on read).
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[derive(Clone, Copy, Debug)]
pub enum CameraAction {
    /// Drag deltas in logical pixels.
    Orbit { dx: f32, dy: f32 },
    /// Drag deltas and viewport height in logical pixels.
    Pan { dx: f32, dy: f32, height: f32 },
    /// `factor < 1` zooms in.
    Zoom { factor: f32 },
    /// Rotate about a screen axis ('x', 'y' or 'z').
    Turn { axis: char, degrees: f32 },
    Reset,
}

#[derive(Debug)]
pub enum Msg {
    Camera(CameraAction),
    /// The surface was resized; draw again at the new size.
    Resized,
    /// The merged scene changed (objects added, removed or moved), or is now empty. `refit`
    /// frames everything; otherwise the camera stays put (e.g. after `align`).
    Scene { structure: Option<Arc<Structure>>, display: Option<Display>, refit: bool },
    /// New display state for the loaded structure.
    Display(Display),
    /// Atoms to mark as selected; empty clears the markers.
    Highlight(Vec<usize>),
    /// Frame these atoms, or the whole structure for `None`.
    Focus(Option<Vec<usize>>),
    /// Measurements to draw; their labels are reported to the UI after every frame.
    Measurements(Vec<Measurement>),
    /// The current camera, for saving sessions.
    GetCamera(Sender<CameraState>),
    /// Restore a saved camera (after the scene that sets its bounds).
    SetCamera(CameraState),
    /// Which atom is drawn at this physical pixel; answered from the latest frame.
    Pick { x: u32, y: u32, reply: Sender<Option<usize>> },
}

/// A measurement label at a physical pixel position (`None` when off screen).
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub name: String,
    pub text: String,
    pub position: Option<[f32; 2]>,
}

/// What the render thread tells the UI.
#[derive(Clone, Debug)]
pub enum ViewEvent {
    Labels(Vec<Label>),
    /// CPU time to encode and submit the last frame, in milliseconds, and the GPU's name.
    Stats { frame_ms: f64, gpu: String },
}

/// Drives the render thread.
#[derive(Clone)]
pub struct Viewport(Sender<Msg>);

impl Viewport {
    pub fn send(&self, msg: Msg) {
        // Fails only once the render thread has stopped, i.e. while the app quits.
        let _ = self.0.send(msg);
    }
}

/// Starts the render thread for `surface`; it reports labels and stats on `events`.
pub fn spawn(surface: WgpuSurfaceHandle, gpu: String, events: UnboundedSender<ViewEvent>) -> Viewport {
    let (tx, rx) = mpsc::channel();
    let state = ViewportState {
        device: surface.device().clone(),
        queue: surface.queue().clone(),
        surface,
        renderer: None,
        size: (0, 0),
        camera: Camera::default(),
        structure: None,
        measurements: Vec::new(),
        labels_shown: false,
        events,
        gpu,
        last_stats: Instant::now() - Duration::from_secs(1),
        frames: 0,
    };
    std::thread::Builder::new().name("viewport".into()).spawn(move || state.run(rx)).expect("spawn the viewport thread");
    // Draw the empty background as soon as the surface has a size.
    let _ = tx.send(Msg::Resized);
    Viewport(tx)
}

struct ViewportState {
    surface: WgpuSurfaceHandle,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Created at the first frame, once the surface has its real size.
    renderer: Option<Renderer>,
    size: (u32, u32),
    camera: Camera,
    structure: Option<Arc<Structure>>,
    measurements: Vec<Measurement>,
    /// Whether the UI currently shows labels, so clearing them is reported once.
    labels_shown: bool,
    events: UnboundedSender<ViewEvent>,
    gpu: String,
    last_stats: Instant,
    frames: u64,
}

impl ViewportState {
    fn run(mut self, rx: Receiver<Msg>) {
        // A frame that could not be drawn (the surface has no size yet) is retried on a short
        // timer; otherwise the thread sleeps until the next message.
        const RETRY: Duration = Duration::from_millis(16);
        // `pending`: the last frame was not drawn. `drawn`: the id buffer matches the scene, so
        // picks can be answered without rendering.
        let mut pending = false;
        let mut drawn = false;
        loop {
            let first = if pending {
                match rx.recv_timeout(RETRY) {
                    Ok(msg) => Some(msg),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            } else {
                // Fails once every sender is dropped, i.e. when the app exits.
                let Ok(msg) = rx.recv() else { return };
                Some(msg)
            };
            let mut picks = Vec::new();
            let mut changed = pending;
            for msg in first.into_iter().chain(rx.try_iter()) {
                match msg {
                    Msg::Pick { x, y, reply } => picks.push((x, y, reply)),
                    msg => {
                        self.apply(msg);
                        changed = true;
                    }
                }
            }
            if changed || !drawn {
                pending = !self.draw();
                drawn = !pending;
            }
            for (x, y, reply) in picks {
                let atom = match (&self.renderer, drawn) {
                    (Some(renderer), true) => renderer.pick(&self.device, &self.queue, x, y),
                    _ => None,
                };
                let _ = reply.send(atom);
            }
        }
    }

    fn aspect(&self) -> f32 {
        let (w, h) = if self.size.0 > 0 { self.size } else { self.surface.size() };
        w as f32 / h.max(1) as f32
    }

    fn fit_camera(&mut self) {
        self.camera = Camera::default();
        if let Some(coords) = self.structure.as_ref().and_then(|s| s.coords(0)) {
            self.camera.fit(coords, self.aspect());
        }
    }

    fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Camera(action) => match action {
                CameraAction::Orbit { dx, dy } => self.camera.orbit(dx, dy),
                CameraAction::Pan { dx, dy, height } => self.camera.pan(dx, dy, height),
                CameraAction::Zoom { factor } => self.camera.zoom(factor),
                CameraAction::Turn { axis, degrees } => {
                    let axis = match axis {
                        'x' => glam::Vec3::X,
                        'y' => glam::Vec3::Y,
                        _ => glam::Vec3::Z,
                    };
                    let turn = glam::Quat::from_axis_angle(axis, degrees.to_radians());
                    self.camera.rotation = (self.camera.rotation * turn).normalize();
                }
                CameraAction::Reset => self.fit_camera(),
            },
            Msg::Resized => {}
            Msg::Scene { structure, display, refit } => {
                let renderer = self.renderer.get_or_insert_with(|| {
                    let (w, h) = self.surface.size();
                    self.size = (w.max(1), h.max(1));
                    Renderer::new(&self.device, FORMAT, w.max(1), h.max(1))
                });
                match (&structure, &display) {
                    (Some(s), Some(d)) => renderer.set_scene(&self.device, s, d, 0),
                    _ => {
                        renderer.clear();
                        self.measurements.clear();
                    }
                }
                self.structure = structure;
                if refit {
                    self.fit_camera();
                }
            }
            Msg::GetCamera(reply) => {
                let c = &self.camera;
                let _ = reply.send(CameraState {
                    target: c.target.into(),
                    distance: c.distance,
                    rotation: c.rotation.into(),
                    fov_y: c.fov_y,
                });
            }
            Msg::SetCamera(state) => {
                self.camera.target = state.target.into();
                self.camera.distance = state.distance;
                self.camera.rotation = glam::Quat::from_array(state.rotation).normalize();
                self.camera.fov_y = state.fov_y;
            }
            Msg::Measurements(measurements) => {
                let coords = self.structure.as_ref().and_then(|s| s.coords(0)).unwrap_or_default();
                let segments: Vec<_> = measurements.iter().flat_map(|m| m.segments(coords)).collect();
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_measurements(&self.device, &segments);
                }
                self.measurements = measurements;
            }
            Msg::Display(display) => {
                if let (Some(structure), Some(renderer)) = (&self.structure, &mut self.renderer) {
                    renderer.set_scene(&self.device, structure, &display, 0);
                }
            }
            Msg::Highlight(atoms) => {
                if let (Some(coords), Some(renderer)) = (self.structure.as_ref().and_then(|s| s.coords(0)), &mut self.renderer) {
                    renderer.set_highlight(&self.device, coords, atoms);
                }
            }
            Msg::Focus(atoms) => {
                let aspect = self.aspect();
                match (atoms, self.structure.as_ref().and_then(|s| s.coords(0))) {
                    (Some(atoms), Some(coords)) => {
                        let points: Vec<_> = atoms.iter().map(|&i| coords[i]).collect();
                        self.camera.focus(&points, aspect);
                    }
                    (None, Some(coords)) => {
                        let rotation = self.camera.rotation;
                        self.camera.fit(coords, aspect);
                        self.camera.rotation = rotation;
                    }
                    _ => {}
                }
            }
            Msg::Pick { .. } => unreachable!("picks are answered in run()"),
        }
    }

    fn emit_labels(&mut self) {
        if self.measurements.is_empty() && !self.labels_shown {
            return;
        }
        let coords = self.structure.as_ref().and_then(|s| s.coords(0)).unwrap_or_default();
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let labels: Vec<Label> = self
            .measurements
            .iter()
            .map(|m| Label { name: m.name.clone(), text: m.label(), position: self.camera.project(m.anchor(coords), w, h) })
            .collect();
        self.labels_shown = !labels.is_empty();
        let _ = self.events.unbounded_send(ViewEvent::Labels(labels));
    }

    /// Returns `false` if the frame could not be drawn and should be retried.
    fn draw(&mut self) -> bool {
        let Some((view, (w, h))) = self.surface.back_view_with_size() else { return false };
        if w == 0 || h == 0 {
            return false;
        }
        let renderer = self.renderer.get_or_insert_with(|| Renderer::new(&self.device, FORMAT, w, h));
        if self.size != (w, h) {
            renderer.resize(&self.device, w, h);
            self.size = (w, h);
        }
        let start = Instant::now();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        renderer.render(&self.queue, &mut encoder, &view, &self.camera);
        self.queue.submit([encoder.finish()]);
        let frame_ms = start.elapsed().as_secs_f64() * 1e3;
        self.surface.present();
        self.frames += 1;
        if self.frames == 1 {
            eprintln!("viewport: {}, first frame {w}x{h}", self.gpu);
        }

        self.emit_labels();
        if self.last_stats.elapsed() > Duration::from_millis(250) {
            self.last_stats = Instant::now();
            let stats = ViewEvent::Stats { frame_ms, gpu: self.gpu.clone() };
            let _ = self.events.unbounded_send(stats);
        }
        true
    }
}
