//! The native viewer window behind the `rsmol` command.
//!
//! It runs on the process's main thread (which windowing on macOS and Linux requires) while
//! Python keeps running on other threads. Commands from Python bump the shared state's version
//! and wake the event loop, which re-reads the scene and applies queued view changes.
//!
//! Controls: left-drag rotates, right-drag (or shift+drag) pans, scroll zooms; hovering shows the
//! atom in the title bar; click selects a residue (shift-click adds); 1–5 apply the display
//! presets, R resets the view, Esc clears the selection.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glam::Quat;
use rsmol_core::session::PRESETS;
use rsmol_render::{Camera, Renderer};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::{Shared, ViewRequest, apply_camera_state, camera_state, default_rotation};

/// Pointer travel (physical pixels) under which a press and release count as a click.
const CLICK_SLOP: f64 = 4.0;
/// `RSMOL_DEBUG=1` logs frame and window events to stderr, for diagnosing blank windows.
fn debug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RSMOL_DEBUG").is_some_and(|v| v != "0"))
}

/// How soon to retry a frame the window system would not show.
const RETRY: Duration = Duration::from_millis(16);

pub fn run(shared: Arc<Shared>, title: &str) -> Result<(), String> {
    let event_loop = EventLoop::<()>::with_user_event().build().map_err(|e| format!("cannot open a window: {e}"))?;
    let proxy = Mutex::new(event_loop.create_proxy());
    *shared.waker.lock().unwrap() = Some(Box::new(move || {
        let _ = proxy.lock().unwrap().send_event(());
    }));
    let mut viewer = Viewer { shared: shared.clone(), title: title.to_owned(), gfx: None, error: None };
    let result = event_loop.run_app(&mut viewer);
    *shared.waker.lock().unwrap() = None;
    shared.lock().viewer_camera = None;
    result.map_err(|e| e.to_string())?;
    viewer.error.map_or(Ok(()), Err)
}

struct Viewer {
    shared: Arc<Shared>,
    title: String,
    gfx: Option<Gfx>,
    /// A setup failure, reported once the event loop exits.
    error: Option<String>,
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    camera: Camera,
    /// Version of the shared state last drawn; `None` before the first sync.
    version: Option<u64>,
    has_structure: bool,
    cursor: Option<PhysicalPosition<f64>>,
    pressed_at: Option<PhysicalPosition<f64>>,
    travel: f64,
    rotating: bool,
    panning: bool,
    modifiers: ModifiersState,
    hover_title: String,
    last_frame: Instant,
    /// The last frame could not be shown (macOS reports a new window as occluded until it is
    /// on screen); it is retried on a short timer instead of waiting for the next event.
    retry: bool,
}

impl Viewer {
    fn setup(&mut self, event_loop: &ActiveEventLoop) -> Result<Gfx, String> {
        let attrs = Window::default_attributes().with_title(&self.title).with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        // Started from a terminal, the window can open behind it; ask to come to the front.
        window.focus_window();
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(event_loop.owned_display_handle())));
        let surface = instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("no GPU adapter for the window: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).map_err(|e| e.to_string())?;
        if debug() {
            let info = adapter.get_info();
            eprintln!("rsmol: GPU {} ({:?})", info.name, info.backend);
        }
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("the window surface is not supported by the GPU")?;
        let caps = surface.get_capabilities(&adapter);
        config.format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(config.format);
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);
        let renderer = Renderer::new(&device, config.format, config.width, config.height);
        let camera = Camera { rotation: default_rotation(), ..Default::default() };
        Ok(Gfx {
            window,
            surface,
            device,
            queue,
            config,
            renderer,
            camera,
            version: None,
            has_structure: false,
            cursor: None,
            pressed_at: None,
            travel: 0.0,
            rotating: false,
            panning: false,
            modifiers: ModifiersState::empty(),
            hover_title: String::new(),
            last_frame: Instant::now(),
            retry: false,
        })
    }
}

impl Gfx {
    fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height.max(1) as f32
    }

    /// Re-reads the scene if the shared state changed, and applies queued view requests.
    fn sync(&mut self, shared: &Shared) {
        let mut state = shared.lock();
        if self.version == Some(state.version) {
            return;
        }
        if debug() {
            eprintln!("rsmol: scene version {} ({} atoms)", state.version, state.core.structure.as_ref().map_or(0, |s| s.atom_count()));
        }
        let first = self.version.is_none();
        self.version = Some(state.version);
        let structure = state.core.structure.clone();
        match (&structure, state.core.render_display()) {
            (Some(s), Some(display)) => {
                self.renderer.set_scene(&self.device, s, &display, 0);
                let coords = s.coords(0).unwrap_or_default();
                let sele: Vec<usize> = state.core.named.get("sele").map(|set| set.iter().collect()).unwrap_or_default();
                self.renderer.set_highlight(&self.device, coords, sele);
                let segments: Vec<_> = state.core.measurements.iter().flat_map(|m| m.segments(coords)).collect();
                self.renderer.set_measurements(&self.device, &segments);
            }
            _ => self.renderer.clear(),
        }
        let newly_loaded = structure.is_some() && !self.has_structure;
        self.has_structure = structure.is_some();
        let mut requests = std::mem::take(&mut state.view_requests);
        drop(state);
        // The first scene is framed even if it was loaded before the window opened.
        if first || newly_loaded {
            requests.insert(0, ViewRequest::Fit);
        }
        let coords = structure.as_ref().and_then(|s| s.coords(0)).unwrap_or_default();
        let aspect = self.aspect();
        for request in requests {
            match request {
                ViewRequest::Fit => self.camera.fit(coords, aspect),
                ViewRequest::Focus(atoms) => {
                    let points: Vec<_> = atoms.iter().filter_map(|&i| coords.get(i).copied()).collect();
                    self.camera.focus(&points, aspect);
                }
                ViewRequest::Turn(axis, degrees) => {
                    self.camera.rotation = (self.camera.rotation * Quat::from_axis_angle(axis, degrees.to_radians())).normalize();
                }
                ViewRequest::Reset => {
                    self.camera.rotation = default_rotation();
                    self.camera.fit(coords, aspect);
                }
                ViewRequest::Saved(saved) => {
                    self.camera.fit(coords, aspect);
                    apply_camera_state(&mut self.camera, &saved);
                }
            }
        }
        self.window.request_redraw();
    }

    fn draw(&mut self, shared: &Shared) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return;
            }
            other => {
                if debug() && !self.retry {
                    eprintln!("rsmol: frame not shown ({}), retrying", match other {
                        wgpu::CurrentSurfaceTexture::Occluded => "window occluded",
                        wgpu::CurrentSurfaceTexture::Timeout => "timeout",
                        _ => "surface error",
                    });
                }
                self.retry = true;
                return;
            }
        };
        if debug() && (self.retry || self.version == Some(0) || self.version.is_none()) {
            eprintln!("rsmol: frame presented ({}x{}, scene version {:?})", self.config.width, self.config.height, self.version);
        }
        self.retry = false;
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(&self.queue, &mut encoder, &view, &self.camera);
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
        self.last_frame = Instant::now();
        // `png` and `save` from Python use exactly this view while the window is open.
        shared.lock().viewer_camera = Some(camera_state(&self.camera));
    }

    /// Runs a command on the shared session (as Python would) and prints its result.
    fn command(&mut self, shared: &Shared, text: &str) {
        let message = {
            let mut state = shared.lock();
            match state.core.run(text, false) {
                Ok(outcome) => {
                    state.apply(&outcome.effects);
                    outcome.message
                }
                Err(e) => e.render(text),
            }
        };
        println!("rsmol> {text}\n  {message}");
        self.sync(shared);
    }

    fn atom_at(&self, p: PhysicalPosition<f64>) -> Option<usize> {
        if p.x < 0.0 || p.y < 0.0 {
            return None;
        }
        self.renderer.pick(&self.device, &self.queue, p.x as u32, p.y as u32)
    }

    fn hover(&mut self, shared: &Shared, title: &str, p: PhysicalPosition<f64>) {
        let label = self.atom_at(p).and_then(|i| {
            let state = shared.lock();
            let s = state.core.structure.as_ref()?;
            (i < s.atom_count()).then(|| {
                format!("{} · {} · B {:.1}", s.atom_label(i), s.atoms.element[i].symbol(), s.atoms.b_factor[i])
            })
        });
        let wanted = label.map_or_else(|| title.to_owned(), |l| format!("{title} — {l}"));
        if wanted != self.hover_title {
            self.window.set_title(&wanted);
            self.hover_title = wanted;
        }
    }
}

impl ApplicationHandler for Viewer {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        match self.setup(event_loop) {
            Ok(mut gfx) => {
                gfx.sync(&self.shared);
                self.gfx = Some(gfx);
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if let (StartCause::ResumeTimeReached { .. }, Some(g)) = (cause, &self.gfx)
            && g.retry
        {
            g.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Sleep until the next event, unless a dropped frame needs another try.
        let retry = self.gfx.as_ref().is_some_and(|g| g.retry);
        event_loop.set_control_flow(if retry { ControlFlow::WaitUntil(Instant::now() + RETRY) } else { ControlFlow::Wait });
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, (): ()) {
        if self.shared.quit.swap(false, std::sync::atomic::Ordering::SeqCst) {
            event_loop.exit();
            return;
        }
        if let Some(g) = &mut self.gfx {
            g.sync(&self.shared);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(g) = &mut self.gfx else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Occluded(false) => g.window.request_redraw(),
            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                g.config.width = size.width;
                g.config.height = size.height;
                g.surface.configure(&g.device, &g.config);
                g.renderer.resize(&g.device, size.width, size.height);
                g.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => g.modifiers = m.state(),
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if pressed {
                    g.pressed_at = g.cursor;
                    g.travel = 0.0;
                    match button {
                        MouseButton::Left if g.modifiers.shift_key() => g.panning = true,
                        MouseButton::Left => g.rotating = true,
                        MouseButton::Right | MouseButton::Middle => g.panning = true,
                        _ => {}
                    }
                    return;
                }
                let was_click = button == MouseButton::Left && g.pressed_at.is_some() && g.travel < CLICK_SLOP;
                g.rotating = false;
                g.panning = false;
                if let (true, Some(p)) = (was_click, g.cursor)
                    && let Some(atom) = g.atom_at(p)
                {
                    // Shift-click extends the selection; a plain click replaces it.
                    let residue = format!("byres index {}", atom + 1);
                    let has_sele = self.shared.lock().core.named.contains_key("sele");
                    let text = if g.modifiers.shift_key() && has_sele {
                        format!("select sele or {residue}")
                    } else {
                        format!("select {residue}")
                    };
                    g.command(&self.shared, &text);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(prev) = g.cursor {
                    let (dx, dy) = ((position.x - prev.x) as f32, (position.y - prev.y) as f32);
                    if g.rotating || g.panning {
                        g.travel += (dx.abs() + dy.abs()) as f64;
                        if g.rotating {
                            g.camera.orbit(dx, dy);
                        } else {
                            g.camera.pan(dx, dy, g.config.height as f32);
                        }
                        g.window.request_redraw();
                    }
                }
                g.cursor = Some(position);
                if !(g.rotating || g.panning) {
                    g.hover(&self.shared, &self.title, position);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                g.camera.zoom(0.9f32.powf(lines));
                g.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => match &event.logical_key {
                Key::Named(NamedKey::Escape) => g.command(&self.shared, "deselect"),
                Key::Character(key) => match key.as_str() {
                    "r" | "R" => {
                        g.camera.rotation = default_rotation();
                        if let Some(s) = self.shared.lock().core.structure.clone() {
                            let aspect = g.aspect();
                            g.camera.fit(s.coords(0).unwrap_or_default(), aspect);
                        }
                        g.window.request_redraw();
                    }
                    k => {
                        if let Some((_, commands)) = k.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| PRESETS.get(i)) {
                            g.command(&self.shared, commands);
                        }
                    }
                },
                _ => {}
            },
            WindowEvent::RedrawRequested => g.draw(&self.shared),
            _ => {}
        }
    }
}
