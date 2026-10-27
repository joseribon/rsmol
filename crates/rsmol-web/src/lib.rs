//! rsmol-render in a browser `<canvas>`: the app's viewport on Linux and Windows, where the
//! native surface cannot sit under the webview. WebGPU where the webview has it, WebGL2
//! otherwise.
//!
//! The app's Rust backend stays the source of truth. It streams the scene here as each
//! object's original file plus its transform (parsed and merged exactly as
//! `Session::rebuild_scene` does), then per-atom display bytes; camera, drawing and picking
//! happen locally, so interaction never waits on IPC.
#![cfg(target_arch = "wasm32")]

use std::collections::HashMap;
use std::sync::Arc;

use rsmol_core::display::{Display, Reps};
use rsmol_core::superpose::Transform;
use rsmol_core::{Structure, Vec3};
use rsmol_render::{Camera, Renderer};
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

fn err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[wasm_bindgen]
pub struct WebRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The format the renderer draws in: an sRGB view of the canvas texture.
    view_format: wgpu::TextureFormat,
    renderer: Renderer,
    camera: Camera,
    /// Parsed files by the key the backend gave them, with their original coordinates.
    sources: HashMap<u32, Arc<Structure>>,
    /// The merged scene, as the backend's `Session::structure`.
    structure: Option<Arc<Structure>>,
    display: Option<Display>,
    gpu: String,
}

#[wasm_bindgen]
impl WebRenderer {
    /// Draws into `canvas`, whose `width`/`height` must already be in physical pixels.
    /// `webgl` skips WebGPU even where the browser offers it.
    pub async fn create(canvas: HtmlCanvasElement, webgl: bool) -> Result<WebRenderer, JsValue> {
        console_error_panic_hook::set_once();
        let (width, height) = (canvas.width().max(1), canvas.height().max(1));
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = if webgl { wgpu::Backends::GL } else { wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL };
        let instance = wgpu::util::new_instance_with_webgpu_detection(desc).await;
        let surface = instance.create_surface(wgpu::SurfaceTarget::Canvas(canvas)).map_err(err)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
                apply_limit_buckets: false,
            })
            .await
            .map_err(err)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor { required_limits: adapter.limits(), ..Default::default() })
            .await
            .map_err(err)?;

        let mut config = surface.get_default_config(&adapter, width, height).ok_or("the canvas is not supported")?;
        // The composite writes linear color, so draw through an sRGB view like the native app.
        let srgb = config.format.add_srgb_suffix();
        if surface.get_capabilities(&adapter).formats.contains(&srgb) {
            config.format = srgb;
        } else if srgb != config.format {
            config.view_formats.push(srgb);
        }
        surface.configure(&device, &config);
        let view_format = config.view_formats.first().copied().unwrap_or(config.format);

        // WebGL2 renders to half floats only with EXT_color_buffer_float.
        let hdr = adapter
            .get_texture_format_features(rsmol_render::SCENE_FORMAT)
            .allowed_usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
        let scene_format = if hdr { rsmol_render::SCENE_FORMAT } else { wgpu::TextureFormat::Rgba8Unorm };
        let renderer = Renderer::with_scene_format(&device, view_format, scene_format, width, height);

        let info = adapter.get_info();
        let backend = if info.backend == wgpu::Backend::BrowserWebGpu { "WebGPU" } else { "WebGL2" };
        let gpu = if info.name.is_empty() { backend.to_owned() } else { format!("{} · {backend}", info.name) };
        Ok(WebRenderer {
            surface,
            device,
            queue,
            config,
            view_format,
            renderer,
            camera: Camera::default(),
            sources: HashMap::new(),
            structure: None,
            display: None,
            gpu,
        })
    }

    /// Adapter name and backend, for the frame-stats chip.
    pub fn gpu(&self) -> String {
        self.gpu.clone()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 && (width, height) != (self.config.width, self.config.height) {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.renderer.resize(&self.device, width, height);
        }
    }

    /// Parses a structure file sent by the backend:
    /// `[key: u32 LE][name length: u32 LE][file name, UTF-8][file bytes]`.
    pub fn add_source(&mut self, message: &[u8]) -> Result<(), JsValue> {
        let u32_at = |at: usize| message.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let (Some(key), Some(len)) = (u32_at(0), u32_at(4)) else { return Err(err("truncated source")) };
        let name_end = 8 + len as usize;
        let name = message.get(8..name_end).and_then(|b| std::str::from_utf8(b).ok()).ok_or("bad source name")?;
        let structure = rsmol_core::io::parse_bytes(&message[name_end..], name).map_err(err)?;
        self.sources.insert(key, Arc::new(structure));
        Ok(())
    }

    /// Replaces the scene: `keys` are sources in object order, `transforms` 12 floats each
    /// (row-major rotation, then translation). Sources not listed are forgotten. Geometry is
    /// rebuilt once the matching display arrives.
    pub fn set_scene(&mut self, keys: &[u32], transforms: &[f32], refit: bool) -> Result<(), JsValue> {
        let mut parts = Vec::with_capacity(keys.len());
        for (k, key) in keys.iter().enumerate() {
            let source = self.sources.get(key).ok_or_else(|| err(format!("unknown source {key}")))?;
            let t = transforms.get(k * 12..k * 12 + 12).ok_or("missing transform")?;
            let transform = Transform {
                rotation: [[t[0], t[1], t[2]], [t[3], t[4], t[5]], [t[6], t[7], t[8]]],
                translation: [t[9], t[10], t[11]],
            };
            parts.push(if transform == Transform::IDENTITY { source.clone() } else { Arc::new(transformed(source, &transform)) });
        }
        self.sources.retain(|key, _| keys.contains(key));
        self.structure = match parts.as_slice() {
            [] => None,
            [only] => Some(only.clone()),
            many => {
                let refs: Vec<&Structure> = many.iter().map(|s| s.as_ref()).collect();
                Some(Arc::new(Structure::concat("scene", &refs)))
            }
        };
        self.display = None;
        if self.structure.is_none() {
            self.renderer.clear();
        }
        if refit {
            self.fit_camera();
        }
        Ok(())
    }

    /// Per-atom display: one byte of representation flags per atom, then RGBA per atom.
    pub fn set_display(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let Some(structure) = &self.structure else { return Ok(()) };
        let n = structure.atom_count();
        if bytes.len() != n * 5 {
            return Err(err(format!("display for {} atoms, scene has {n}", bytes.len() / 5)));
        }
        let (reps, colors) = bytes.split_at(n);
        let display = Display {
            reps: reps.iter().map(|&b| Reps::from_bits(b)).collect(),
            colors: colors.as_chunks::<4>().0.to_vec(),
        };
        self.renderer.set_scene(&self.device, structure, &display, 0);
        self.display = Some(display);
        Ok(())
    }

    /// Marks atoms (0-based) as selected; empty clears.
    pub fn set_highlight(&mut self, atoms: &[u32]) {
        let coords = self.structure.as_ref().and_then(|s| s.coords(0)).unwrap_or_default();
        let atoms = atoms.iter().map(|&i| i as usize).filter(|&i| i < coords.len());
        self.renderer.set_highlight(&self.device, coords, atoms);
    }

    /// Dashed measurement lines, 6 floats (two points) per segment.
    pub fn set_measurements(&mut self, segments: &[f32]) {
        let segments: Vec<(Vec3, Vec3)> =
            segments.as_chunks::<6>().0.iter().map(|s| ([s[0], s[1], s[2]], [s[3], s[4], s[5]])).collect();
        self.renderer.set_measurements(&self.device, &segments);
    }

    /// Drag deltas in CSS pixels.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.camera.orbit(dx, dy);
    }

    /// Drag deltas and viewport height in CSS pixels.
    pub fn pan(&mut self, dx: f32, dy: f32, height: f32) {
        self.camera.pan(dx, dy, height);
    }

    /// `factor < 1` zooms in.
    pub fn zoom(&mut self, factor: f32) {
        self.camera.zoom(factor);
    }

    /// Rotates about a screen axis ('x', 'y' or 'z').
    pub fn turn(&mut self, axis: char, degrees: f32) {
        let axis = match axis {
            'x' => glam::Vec3::X,
            'y' => glam::Vec3::Y,
            _ => glam::Vec3::Z,
        };
        let turn = glam::Quat::from_axis_angle(axis, degrees.to_radians());
        self.camera.rotation = (self.camera.rotation * turn).normalize();
    }

    /// Back to the default orientation, framing everything.
    pub fn reset(&mut self) {
        self.fit_camera();
    }

    /// Frames these atoms (0-based), or everything (keeping the rotation) when empty.
    pub fn focus(&mut self, atoms: &[u32]) {
        let aspect = self.aspect();
        let Some(coords) = self.structure.as_ref().and_then(|s| s.coords(0)) else { return };
        if atoms.is_empty() {
            let rotation = self.camera.rotation;
            self.camera.fit(coords, aspect);
            self.camera.rotation = rotation;
        } else {
            let points: Vec<_> = atoms.iter().filter_map(|&i| coords.get(i as usize).copied()).collect();
            self.camera.focus(&points, aspect);
        }
    }

    /// target (3), distance, rotation quaternion (4), vertical field of view.
    pub fn camera_state(&self) -> Vec<f32> {
        let c = &self.camera;
        let mut out = c.target.to_array().to_vec();
        out.push(c.distance);
        out.extend(c.rotation.to_array());
        out.push(c.fov_y);
        out
    }

    /// Restores a `camera_state` (after the scene, which sets the clip bounds).
    pub fn set_camera_state(&mut self, s: &[f32]) {
        if let [x, y, z, distance, qx, qy, qz, qw, fov_y] = *s {
            self.camera.target = glam::Vec3::new(x, y, z);
            self.camera.distance = distance;
            self.camera.rotation = glam::Quat::from_xyzw(qx, qy, qz, qw).normalize();
            self.camera.fov_y = fov_y;
        }
    }

    /// Physical pixel positions of world points (3 floats each), NaN where off screen.
    pub fn project(&self, points: &[f32]) -> Vec<f32> {
        let (w, h) = (self.config.width as f32, self.config.height as f32);
        points
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&p| self.camera.project(p, w, h).unwrap_or([f32::NAN; 2]))
            .collect()
    }

    /// Draws a frame; returns the CPU time it took in milliseconds.
    pub fn render(&mut self) -> Result<f64, JsValue> {
        let start = js_sys::Date::now();
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(0.0);
            }
            other => return Err(err(format!("canvas texture: {other:?}"))),
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..Default::default()
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(&self.queue, &mut encoder, &view, &self.camera);
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(js_sys::Date::now() - start)
    }

    /// The atom (0-based) drawn at physical pixel (`x`, `y`) in the last frame, or -1.
    pub fn pick(&self, x: f32, y: f32) -> js_sys::Promise {
        let pending = (x >= 0.0 && y >= 0.0).then(|| self.renderer.pick_async(&self.device, &self.queue, x as u32, y as u32));
        let device = self.device.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let atom = match pending {
                Some(fut) => drive(&device, fut).await,
                None => None,
            };
            Ok(JsValue::from(atom.map_or(-1, |i| i as i32)))
        })
    }
}

impl WebRenderer {
    fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height.max(1) as f32
    }

    fn fit_camera(&mut self) {
        self.camera = Camera::default();
        let aspect = self.aspect();
        if let Some(coords) = self.structure.as_ref().and_then(|s| s.coords(0)) {
            self.camera.fit(coords, aspect);
        }
    }
}

/// As `Session` applies an object's transform to its file's coordinates.
fn transformed(s: &Structure, t: &Transform) -> Structure {
    let mut out = s.clone();
    for state in &mut out.states {
        for p in state.iter_mut() {
            *p = t.apply(*p);
        }
    }
    out
}

/// Runs a GPU readback to completion. WebGL buffers map only when the device is polled,
/// which the browser never does for us, so poll between turns of the event loop. (WebGPU
/// resolves on its own; polling it does nothing.)
async fn drive<T>(device: &wgpu::Device, fut: impl Future<Output = T>) -> T {
    let mut fut = std::pin::pin!(fut);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        let _ = device.poll(wgpu::PollType::Poll);
        if let std::task::Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        let tick = js_sys::Promise::new(&mut |resolve, _| {
            let _ = web_sys::window().expect("a window").set_timeout_with_callback(&resolve);
        });
        let _ = wasm_bindgen_futures::JsFuture::from(tick).await;
    }
}
