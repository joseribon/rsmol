//! Standalone prototype viewer.
//!
//!   rsmol-viewer <structure.{pdb,cif}>                     interactive window
//!   rsmol-viewer <structure> --screenshot out.png [WxH]    render offscreen and exit
//!
//! Controls: left-drag rotates, right-drag (or shift+left-drag) pans, scroll zooms,
//! R resets the view.

use std::sync::Arc;
use std::time::Instant;

use rsmol_core::Structure;
use rsmol_render::{Camera, Renderer};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Window, WindowId};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: rsmol-viewer <structure.pdb|cif> [--screenshot out.png [WxH]]");
        std::process::exit(2);
    };
    let t = Instant::now();
    let structure = rsmol_core::io::read(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    println!(
        "{}: {} atoms, {} residues, {} states, parsed in {:.1?}",
        structure.name,
        structure.atom_count(),
        structure.residues.len(),
        structure.state_count(),
        t.elapsed()
    );

    if let Some(i) = args.iter().position(|a| a == "--screenshot") {
        let out = args.get(i + 1).expect("--screenshot needs an output path");
        let (w, h) = args
            .get(i + 2)
            .and_then(|s| s.split_once('x'))
            .map(|(w, h)| (w.parse().unwrap(), h.parse().unwrap()))
            .unwrap_or((1600, 1000));
        pollster::block_on(screenshot(&structure, out, w, h));
        return;
    }

    let event_loop = EventLoop::new().unwrap();
    let mut app = App { structure, gfx: None };
    event_loop.run_app(&mut app).unwrap();
}

fn fitted_camera(structure: &Structure, width: u32, height: u32) -> Camera {
    let mut camera = Camera::default();
    camera.fit(structure.coords(0).unwrap_or_default(), width as f32 / height.max(1) as f32);
    camera
}

fn adapter_options<'a>(surface: Option<&'a wgpu::Surface<'a>>) -> wgpu::RequestAdapterOptions<'a, 'a> {
    wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: surface,
        apply_limit_buckets: false,
    }
}

struct App {
    structure: Structure,
    gfx: Option<Gfx>,
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    camera: Camera,
    input: Input,
    fps: FpsCounter,
    name: String,
    atoms: usize,
}

#[derive(Default)]
struct Input {
    cursor: Option<PhysicalPosition<f64>>,
    rotating: bool,
    panning: bool,
    modifiers: ModifiersState,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(format!("rsmol — {}", self.structure.name))
            .with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&adapter_options(Some(&surface)))).unwrap();
        println!("GPU: {} ({:?})", adapter.get_info().name, adapter.get_info().backend);
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();

        let size = window.inner_size();
        let mut config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)).unwrap();
        // Prefer an sRGB format so shader output (linear) is encoded correctly.
        let caps = surface.get_capabilities(&adapter);
        config.format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(config.format);
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);

        let mut renderer = Renderer::new(&device, config.format, config.width, config.height);
        renderer.set_structure(&device, &self.structure, 0);
        let camera = fitted_camera(&self.structure, config.width, config.height);
        self.gfx = Some(Gfx {
            window,
            surface,
            device,
            queue,
            config,
            renderer,
            camera,
            input: Input::default(),
            fps: FpsCounter::new(),
            name: self.structure.name.clone(),
            atoms: self.structure.atom_count(),
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(g) = &mut self.gfx else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                g.config.width = size.width;
                g.config.height = size.height;
                g.surface.configure(&g.device, &g.config);
                g.renderer.resize(&g.device, size.width, size.height);
                g.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => g.input.modifiers = m.state(),
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                match button {
                    MouseButton::Left if g.input.modifiers.shift_key() => g.input.panning = pressed,
                    MouseButton::Left => g.input.rotating = pressed,
                    MouseButton::Right | MouseButton::Middle => g.input.panning = pressed,
                    _ => {}
                }
                if !pressed {
                    g.input.rotating = false;
                    g.input.panning = false;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(prev) = g.input.cursor {
                    let (dx, dy) = ((position.x - prev.x) as f32, (position.y - prev.y) as f32);
                    if g.input.rotating {
                        g.camera.orbit(dx, dy);
                    } else if g.input.panning {
                        g.camera.pan(dx, dy, g.config.height as f32);
                    }
                    if g.input.rotating || g.input.panning {
                        g.window.request_redraw();
                    }
                }
                g.input.cursor = Some(position);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                g.camera.zoom(0.9f32.powf(lines));
                g.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if event.logical_key == Key::Character("r".into()) {
                    g.camera = fitted_camera(&self.structure, g.config.width, g.config.height);
                    g.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => g.draw(),
            _ => {}
        }
    }
}

impl Gfx {
    fn draw(&mut self) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return;
            }
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(&self.queue, &mut encoder, &view, &self.camera);
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);

        if let Some(fps) = self.fps.tick() {
            self.window.set_title(&format!("rsmol — {} — {} atoms — {fps:.0} fps", self.name, self.atoms));
        }
    }
}

/// Frame rate over the last second of *rendered* frames (redraws only happen on input).
struct FpsCounter {
    start: Instant,
    frames: u32,
}

impl FpsCounter {
    fn new() -> Self {
        FpsCounter { start: Instant::now(), frames: 0 }
    }

    fn tick(&mut self) -> Option<f32> {
        self.frames += 1;
        let elapsed = self.start.elapsed().as_secs_f32();
        (elapsed >= 1.0).then(|| {
            let fps = self.frames as f32 / elapsed;
            self.start = Instant::now();
            self.frames = 0;
            fps
        })
    }
}

/// Renders one frame offscreen and writes it as a PNG.
async fn screenshot(structure: &Structure, out: &str, width: u32, height: u32) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance.request_adapter(&adapter_options(None)).await.unwrap();
    let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

    let mut renderer = Renderer::new(&device, format, width, height);
    renderer.set_structure(&device, structure, 0);
    let mut camera = fitted_camera(structure, width, height);
    // A slight tilt reads better than the raw file orientation.
    camera.orbit(-120.0, -60.0);

    let row_bytes = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (row_bytes * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let t = Instant::now();
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(&queue, &mut encoder, &target.create_view(&Default::default()), &camera);
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row_bytes), rows_per_image: None },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    readback.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    println!("first frame (incl. pipeline warm-up): {:.1?} on {}", t.elapsed(), adapter.get_info().name);

    // Steady state: render and wait for the GPU each time, so this is the full frame latency.
    const FRAMES: u32 = 100;
    let view = target.create_view(&Default::default());
    let t = Instant::now();
    for i in 0..FRAMES {
        camera.orbit(if i % 2 == 0 { 1.0 } else { -1.0 }, 0.0);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(&queue, &mut encoder, &view, &camera);
        queue.submit([encoder.finish()]);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }
    let per_frame = t.elapsed() / FRAMES;
    println!("steady state: {per_frame:.2?}/frame ({:.0} fps) at {width}x{height}", 1.0 / per_frame.as_secs_f64());

    let data = readback.slice(..).get_mapped_range().unwrap();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in data.chunks(row_bytes as usize) {
        pixels.extend_from_slice(&row[..(width * 4) as usize]);
    }
    let file = std::io::BufWriter::new(std::fs::File::create(out).unwrap());
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
    println!("wrote {out}");
}
