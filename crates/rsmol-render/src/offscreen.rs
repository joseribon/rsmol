//! Headless rendering to an RGBA image, for scripts and batch figures.

use rsmol_core::Structure;
use rsmol_core::display::Display;

use crate::{Camera, Renderer, Vec3};

/// A GPU device and renderer drawing into an offscreen texture.
pub struct Offscreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    target: wgpu::Texture,
    size: (u32, u32),
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

impl Offscreen {
    pub fn new(width: u32, height: u32) -> Result<Offscreen, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).map_err(|e| format!("no GPU device: {e}"))?;
        let renderer = Renderer::new(&device, FORMAT, width, height);
        let target = create_target(&device, width, height);
        Ok(Offscreen { device, queue, renderer, target, size: (width, height) })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if (width, height) != self.size {
            self.target = create_target(&self.device, width, height);
            self.renderer.resize(&self.device, width, height);
            self.size = (width, height);
        }
    }

    /// Uploads the scene: geometry, selection markers and measurement segments.
    pub fn set_scene(&mut self, s: &Structure, display: &Display, highlight: &[usize], segments: &[(Vec3, Vec3)]) {
        self.renderer.set_scene(&self.device, s, display, 0);
        self.renderer.set_highlight(&self.device, s.coords(0).unwrap_or_default(), highlight.iter().copied());
        self.renderer.set_measurements(&self.device, segments);
    }

    /// Renders one frame and returns tightly packed RGBA rows, top to bottom.
    pub fn render(&self, camera: &Camera) -> Vec<u8> {
        let (width, height) = self.size;
        let row_bytes = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("offscreen readback"),
            size: (row_bytes * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(&self.queue, &mut encoder, &self.target.create_view(&Default::default()), camera);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
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
        self.queue.submit([encoder.finish()]);
        readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = readback.slice(..).get_mapped_range().expect("readback was mapped");
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for row in data.chunks(row_bytes as usize) {
            pixels.extend_from_slice(&row[..(width * 4) as usize]);
        }
        pixels
    }
}

fn create_target(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen target"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
