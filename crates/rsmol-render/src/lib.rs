//! wgpu renderer for rsmol.
//!
//! Window-system agnostic: the host (winit viewer, Tauri shell, browser canvas) owns the surface
//! and hands the renderer a texture view to draw into each frame.

mod camera;
pub mod style;

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use rsmol_core::Structure;
use wgpu::util::DeviceExt;

pub use camera::Camera;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    view: [[f32; 4]; 4],
    proj: [[f32; 4]; 4],
}

/// Per-atom GPU data for the sphere representation: 20 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SphereInstance {
    center: [f32; 3],
    radius: f32,
    color: [u8; 4],
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    spheres: Option<(wgpu::Buffer, u32)>,
    depth: wgpu::TextureView,
    size: (u32, u32),
    pub clear_color: wgpu::Color,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat, width: u32, height: u32) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("sphere.wgsl"));
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniforms"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("spheres"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spheres"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<SphereInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Unorm8x4],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(color_format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

        Renderer {
            pipeline,
            uniforms,
            bind_group,
            spheres: None,
            depth: create_depth(device, width, height),
            size: (width, height),
            // Near-black slate; the render target is sRGB so these are linear values.
            clear_color: wgpu::Color { r: 0.008, g: 0.010, b: 0.014, a: 1.0 },
        }
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if (width, height) != self.size && width > 0 && height > 0 {
            self.depth = create_depth(device, width, height);
            self.size = (width, height);
        }
    }

    /// Uploads `structure` as van der Waals spheres; carbons are colored by author chain.
    pub fn set_structure(&mut self, device: &wgpu::Device, structure: &Structure, state: usize) {
        let Some(coords) = structure.coords(state) else {
            self.spheres = None;
            return;
        };
        let mut chain_index: HashMap<&str, usize> = HashMap::new();
        let instances: Vec<SphereInstance> = coords
            .iter()
            .enumerate()
            .map(|(i, &center)| {
                let element = structure.atoms.element[i];
                let color = style::element_color(element).unwrap_or_else(|| {
                    let chain = structure.chain_of(i).id.as_str();
                    let next = chain_index.len();
                    style::chain_color(*chain_index.entry(chain).or_insert(next))
                });
                SphereInstance { center, radius: style::vdw_radius(element), color }
            })
            .collect();
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sphere instances"),
            contents: bytemuck::cast_slice(&instances),
            usage: wgpu::BufferUsages::VERTEX,
        });
        self.spheres = Some((buffer, instances.len() as u32));
    }

    pub fn render(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, camera: &Camera) {
        let aspect = self.size.0 as f32 / self.size.1.max(1) as f32;
        let uniforms = Uniforms {
            view: camera.view().to_cols_array_2d(),
            proj: camera.proj(aspect).to_cols_array_2d(),
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(self.clear_color), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some((buffer, count)) = &self.spheres {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..4, 0..*count);
        }
    }
}

fn create_depth(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}
