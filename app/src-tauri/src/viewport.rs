//! Native wgpu viewport drawn into the Tauri window, underneath the transparent webview.
//!
//! All GPU state lives on a dedicated render thread fed by a channel. Each wakeup drains every
//! pending message before drawing one frame, so a burst of mouse events costs a single redraw
//! and the UI thread never waits on the GPU.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use rsmol_core::Structure;
use rsmol_core::display::Display;
use rsmol_render::{Camera, Renderer};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, WebviewWindow};

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CameraAction {
    /// Drag deltas in CSS pixels.
    Orbit { dx: f32, dy: f32 },
    /// Drag deltas and viewport height in CSS pixels.
    Pan { dx: f32, dy: f32, height: f32 },
    /// `factor < 1` zooms in.
    Zoom { factor: f32 },
    Reset,
}

#[derive(Debug)]
pub enum Msg {
    Camera(CameraAction),
    Resize { width: u32, height: u32 },
    /// A new structure and how to display it; resets the camera and markers.
    Load(Arc<Structure>, Display),
    /// New display state for the loaded structure.
    Display(Display),
    /// Atoms to mark as selected; empty clears the markers.
    Highlight(Vec<usize>),
    /// Frame these atoms, or the whole structure for `None`.
    Focus(Option<Vec<usize>>),
    /// Which atom is drawn at this physical pixel; answered from the latest frame.
    Pick { x: u32, y: u32, reply: Sender<Option<usize>> },
}

#[derive(Clone, Serialize)]
struct FrameStats {
    /// CPU time to encode and submit the last frame, in milliseconds.
    frame_ms: f64,
    gpu: String,
}

/// Creates the GPU surface on the calling (main) thread, then hands everything to the render
/// thread. Returns the channel that drives it.
pub fn spawn(window: &WebviewWindow, app: AppHandle) -> Result<Sender<Msg>, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    #[cfg(target_os = "macos")]
    // SAFETY: the target view is owned by the window, which outlives the render thread.
    let surface = unsafe { instance.create_surface_unsafe(crate::macos::metal_host_view(window)?) };
    #[cfg(not(target_os = "macos"))]
    let surface = instance.create_surface(window.clone());
    let surface = surface.map_err(|e| format!("create surface: {e}"))?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: Some(&surface),
        apply_limit_buckets: false,
    }))
    .map_err(|e| format!("request adapter: {e}"))?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&Default::default())).map_err(|e| format!("request device: {e}"))?;

    let size = window.inner_size().map_err(|e| e.to_string())?;
    let mut config = surface
        .get_default_config(&adapter, size.width.max(1), size.height.max(1))
        .ok_or("surface is not supported by the adapter")?;
    let caps = surface.get_capabilities(&adapter);
    config.format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(config.format);
    config.present_mode = wgpu::PresentMode::AutoVsync;
    surface.configure(&device, &config);

    let renderer = Renderer::new(&device, config.format, config.width, config.height);
    let gpu = adapter.get_info().name;
    eprintln!("viewport: {gpu}, {:?} {}x{}", config.format, config.width, config.height);
    let (tx, rx) = mpsc::channel();
    let state = ViewportState {
        surface,
        device,
        queue,
        config,
        renderer,
        camera: Camera::default(),
        structure: None,
        app,
        gpu,
        last_stats: Instant::now() - Duration::from_secs(1),
        frames: 0,
    };
    std::thread::Builder::new()
        .name("viewport".into())
        .spawn(move || state.run(rx))
        .map_err(|e| e.to_string())?;
    // Draw the empty background immediately.
    let _ = tx.send(Msg::Resize { width: size.width, height: size.height });
    Ok(tx)
}

struct ViewportState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    camera: Camera,
    structure: Option<Arc<Structure>>,
    app: AppHandle,
    gpu: String,
    last_stats: Instant,
    frames: u64,
}

impl ViewportState {
    fn run(mut self, rx: Receiver<Msg>) {
        // A frame that could not be shown (window not yet visible, minimized, …) is retried
        // on a short timer; otherwise the thread sleeps until the next message.
        const RETRY: Duration = Duration::from_millis(16);
        // `pending`: the last frame was not shown. `drawn`: the id buffer matches the scene, so
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
                let atom = if drawn { self.renderer.pick(&self.device, &self.queue, x, y) } else { None };
                let _ = reply.send(atom);
            }
        }
    }

    fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height.max(1) as f32
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
                CameraAction::Reset => self.fit_camera(),
            },
            Msg::Resize { width, height } => {
                if width > 0 && height > 0 {
                    self.config.width = width;
                    self.config.height = height;
                    self.surface.configure(&self.device, &self.config);
                    self.renderer.resize(&self.device, width, height);
                }
            }
            Msg::Load(structure, display) => {
                self.renderer.set_scene(&self.device, &structure, &display, 0);
                self.renderer.set_highlight(&self.device, &[], []);
                self.structure = Some(structure);
                self.fit_camera();
            }
            Msg::Display(display) => {
                if let Some(structure) = &self.structure {
                    self.renderer.set_scene(&self.device, structure, &display, 0);
                }
            }
            Msg::Highlight(atoms) => {
                if let Some(coords) = self.structure.as_ref().and_then(|s| s.coords(0)) {
                    self.renderer.set_highlight(&self.device, coords, atoms);
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

    /// Returns `false` if the frame could not be presented and should be retried.
    fn draw(&mut self) -> bool {
        let start = Instant::now();
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => return false,
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("viewport: surface validation error");
                return true;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(&self.queue, &mut encoder, &view, &self.camera);
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        self.frames += 1;
        if self.frames == 1 {
            eprintln!("viewport: first frame presented ({}x{})", self.config.width, self.config.height);
        }

        if self.last_stats.elapsed() > Duration::from_millis(250) {
            self.last_stats = Instant::now();
            let stats = FrameStats { frame_ms: start.elapsed().as_secs_f64() * 1e3, gpu: self.gpu.clone() };
            let _ = self.app.emit("viewport-frame", stats);
        }
        true
    }
}
