//! wgpu renderer for rsmol.
//!
//! Window-system agnostic: the host (winit viewer, Tauri shell, browser canvas) owns the surface
//! and hands the renderer a texture view to draw into each frame.

mod camera;
pub mod cartoon;
pub mod style;

use std::borrow::Cow;
use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use rsmol_core::{EntityKind, Structure, backbone};
use wgpu::util::DeviceExt;

pub use camera::Camera;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// What to draw for the loaded structure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Representation {
    /// Every atom as a van der Waals sphere.
    Spheres,
    /// Polymer backbones only.
    Cartoon,
    /// Cartoon for polymers plus spheres for ligands and ions (no water).
    #[default]
    CartoonAndLigands,
}

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

struct Instances {
    buffer: wgpu::Buffer,
    count: u32,
}

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

pub struct Renderer {
    sphere_pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    all_spheres: Option<Instances>,
    ligand_spheres: Option<Instances>,
    cartoon: Option<GpuMesh>,
    depth: wgpu::TextureView,
    size: (u32, u32),
    pub representation: Representation,
    pub clear_color: wgpu::Color,
}

fn shader(device: &wgpu::Device, label: &str, source: &'static str) -> wgpu::ShaderModule {
    let full = format!("{}\n{source}", include_str!("common.wgsl"));
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(full)),
    })
}

impl Renderer {
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat, width: u32, height: u32) -> Self {
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
            label: Some("scene"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let pipeline = |label: &str,
                        module: &wgpu::ShaderModule,
                        buffer: wgpu::VertexBufferLayout,
                        topology: wgpu::PrimitiveTopology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(buffer)],
                },
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(color_format.into())],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let sphere_pipeline = pipeline(
            "spheres",
            &shader(device, "spheres", include_str!("sphere.wgsl")),
            wgpu::VertexBufferLayout {
                array_stride: size_of::<SphereInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Unorm8x4],
            },
            wgpu::PrimitiveTopology::TriangleStrip,
        );
        let mesh_pipeline = pipeline(
            "mesh",
            &shader(device, "mesh", include_str!("mesh.wgsl")),
            wgpu::VertexBufferLayout {
                array_stride: size_of::<cartoon::MeshVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Unorm8x4],
            },
            wgpu::PrimitiveTopology::TriangleList,
        );

        Renderer {
            sphere_pipeline,
            mesh_pipeline,
            uniforms,
            bind_group,
            all_spheres: None,
            ligand_spheres: None,
            cartoon: None,
            depth: create_depth(device, width, height),
            size: (width, height),
            representation: Representation::default(),
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

    /// Uploads every representation of `structure` so switching between them is instant.
    /// Carbons and cartoons are colored by author chain.
    pub fn set_structure(&mut self, device: &wgpu::Device, structure: &Structure, state: usize) {
        self.all_spheres = None;
        self.ligand_spheres = None;
        self.cartoon = None;
        let Some(coords) = structure.coords(state) else { return };

        let mut auth_index: HashMap<&str, usize> = HashMap::new();
        let chain_colors: Vec<[u8; 4]> = structure
            .chains
            .iter()
            .map(|c| {
                let next = auth_index.len();
                style::chain_color(*auth_index.entry(c.id.as_str()).or_insert(next))
            })
            .collect();

        let traces = backbone::traces(structure, state);
        let mut in_trace = vec![false; structure.residues.len()];
        for r in traces.iter().flat_map(|t| &t.residues) {
            in_trace[*r as usize] = true;
        }

        let sphere = |i: usize| {
            let element = structure.atoms.element[i];
            let residue = structure.atoms.residue[i] as usize;
            let color = style::element_color(element)
                .unwrap_or(chain_colors[structure.residues[residue].chain as usize]);
            SphereInstance { center: coords[i], radius: style::vdw_radius(element), color }
        };
        let all: Vec<SphereInstance> = (0..coords.len()).map(sphere).collect();
        let ligands: Vec<SphereInstance> = (0..coords.len())
            .filter(|&i| {
                let residue = structure.atoms.residue[i] as usize;
                !in_trace[residue] && !is_water(structure, residue)
            })
            .map(sphere)
            .collect();
        self.all_spheres = upload_instances(device, "spheres", &all);
        self.ligand_spheres = upload_instances(device, "ligand spheres", &ligands);

        let mesh = cartoon::build(structure, state, &traces, |t| chain_colors[t.chain as usize]);
        if !mesh.indices.is_empty() {
            self.cartoon = Some(GpuMesh {
                vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("cartoon vertices"),
                    contents: bytemuck::cast_slice(&mesh.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("cartoon indices"),
                    contents: bytemuck::cast_slice(&mesh.indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
                count: mesh.indices.len() as u32,
            });
        }
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
        pass.set_bind_group(0, &self.bind_group, &[]);

        // Structures without polymers (small molecules) have no cartoon; show their atoms.
        let (cartoon, spheres) = match (self.representation, &self.cartoon) {
            (Representation::Spheres, _) | (_, None) => (None, self.all_spheres.as_ref()),
            (Representation::Cartoon, Some(c)) => (Some(c), None),
            (Representation::CartoonAndLigands, Some(c)) => (Some(c), self.ligand_spheres.as_ref()),
        };
        if let Some(mesh) = cartoon {
            pass.set_pipeline(&self.mesh_pipeline);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.count, 0, 0..1);
        }
        if let Some(spheres) = spheres {
            pass.set_pipeline(&self.sphere_pipeline);
            pass.set_vertex_buffer(0, spheres.buffer.slice(..));
            pass.draw(0..4, 0..spheres.count);
        }
    }
}

fn is_water(s: &Structure, residue: usize) -> bool {
    let r = &s.residues[residue];
    let entity = s.chains[r.chain as usize].entity.map(|e| s.entities[e as usize].kind);
    entity == Some(EntityKind::Water) || matches!(r.name.as_str(), "HOH" | "WAT" | "DOD" | "H2O" | "TIP3")
}

fn upload_instances(device: &wgpu::Device, label: &str, instances: &[SphereInstance]) -> Option<Instances> {
    (!instances.is_empty()).then(|| Instances {
        buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(instances),
            usage: wgpu::BufferUsages::VERTEX,
        }),
        count: instances.len() as u32,
    })
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
