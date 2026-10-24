//! wgpu renderer for rsmol.
//!
//! Window-system agnostic: the host (winit viewer, Tauri shell, browser canvas) owns the surface
//! and hands the renderer a texture view to draw into each frame.

pub mod atoms;
mod camera;
pub mod cartoon;
pub mod style;

use std::borrow::Cow;
use std::collections::HashMap;

use atoms::{SphereInstance, StickInstance};
use bytemuck::{Pod, Zeroable};
use rsmol_core::{Structure, backbone};
use wgpu::util::DeviceExt;

pub use camera::Camera;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// What to draw for the loaded structure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Representation {
    /// Every atom as a van der Waals sphere.
    Spheres,
    /// Every non-water atom as licorice sticks; ions as small spheres.
    Sticks,
    /// Polymer backbones only.
    Cartoon,
    /// Cartoon for polymers plus sticks for ligands and small spheres for ions (no water).
    #[default]
    CartoonAndLigands,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    view: [[f32; 4]; 4],
    proj: [[f32; 4]; 4],
}

struct Instances {
    buffer: wgpu::Buffer,
    count: u32,
}

/// Bond cylinders plus the spheres that round off their joints (and mark ions).
#[derive(Default)]
struct StickSet {
    joints: Option<Instances>,
    bonds: Option<Instances>,
}

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

pub struct Renderer {
    sphere_pipeline: wgpu::RenderPipeline,
    stick_pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    highlight_pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    all_spheres: Option<Instances>,
    all_sticks: StickSet,
    ligand_sticks: StickSet,
    cartoon: Option<GpuMesh>,
    highlight: Option<Instances>,
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

        // `overlay` pipelines draw on top of the scene: no depth test or write, alpha blended.
        let pipeline = |label: &str,
                        module: &wgpu::ShaderModule,
                        buffer: wgpu::VertexBufferLayout,
                        topology: wgpu::PrimitiveTopology,
                        overlay: bool| {
            let constants: &[(&str, f64)] = if overlay { &[("ALPHA", 0.8)] } else { &[] };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions { constants, ..Default::default() },
                    buffers: &[Some(buffer)],
                },
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(!overlay),
                    depth_compare: Some(if overlay { wgpu::CompareFunction::Always } else { wgpu::CompareFunction::Less }),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions { constants, ..Default::default() },
                    targets: &[Some(wgpu::ColorTargetState {
                        format: color_format,
                        blend: overlay.then_some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
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
            false,
        );
        let highlight_pipeline = pipeline(
            "selection markers",
            &shader(device, "spheres", include_str!("sphere.wgsl")),
            wgpu::VertexBufferLayout {
                array_stride: size_of::<SphereInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Unorm8x4],
            },
            wgpu::PrimitiveTopology::TriangleStrip,
            true,
        );
        let stick_pipeline = pipeline(
            "sticks",
            &shader(device, "sticks", include_str!("stick.wgsl")),
            wgpu::VertexBufferLayout {
                array_stride: size_of::<StickInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x3, 1 => Float32x3, 2 => Float32, 3 => Unorm8x4, 4 => Unorm8x4
                ],
            },
            wgpu::PrimitiveTopology::TriangleStrip,
            false,
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
            false,
        );

        Renderer {
            sphere_pipeline,
            stick_pipeline,
            mesh_pipeline,
            highlight_pipeline,
            uniforms,
            bind_group,
            all_spheres: None,
            all_sticks: StickSet::default(),
            ligand_sticks: StickSet::default(),
            cartoon: None,
            highlight: None,
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
    /// Carbons and cartoons are colored by author chain; ligand carbons are light gray so they
    /// stand out against the cartoon.
    pub fn set_structure(&mut self, device: &wgpu::Device, structure: &Structure, state: usize) {
        self.all_spheres = None;
        self.all_sticks = StickSet::default();
        self.ligand_sticks = StickSet::default();
        self.cartoon = None;
        self.highlight = None;
        let Some(coords) = structure.coords(state) else { return };
        let n = coords.len();

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
        let residue_of = |i: usize| structure.atoms.residue[i] as usize;
        let water: Vec<bool> = (0..n).map(|i| structure.is_water(residue_of(i))).collect();
        let ligand: Vec<bool> = (0..n).map(|i| !in_trace[residue_of(i)] && !water[i]).collect();
        let not_water: Vec<bool> = water.iter().map(|w| !w).collect();

        let color = |i: usize| {
            style::element_color(structure.atoms.element[i])
                .unwrap_or(chain_colors[structure.residues[residue_of(i)].chain as usize])
        };
        let ligand_color =
            |i: usize| style::element_color(structure.atoms.element[i]).unwrap_or(style::LIGAND_CARBON);

        self.all_spheres = upload_instances(device, "spheres", &atoms::spheres(structure, coords, &vec![true; n], color));
        self.all_sticks = upload_sticks(device, "all", atoms::sticks(structure, coords, &not_water, color));
        self.ligand_sticks = upload_sticks(device, "ligand", atoms::sticks(structure, coords, &ligand, ligand_color));

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

    /// Marks `atoms` (of the structure last passed to `set_structure`) as selected; an empty
    /// iterator clears the markers.
    pub fn set_highlight(&mut self, device: &wgpu::Device, coords: &[rsmol_core::Vec3], atoms: impl IntoIterator<Item = usize>) {
        let markers: Vec<SphereInstance> = atoms
            .into_iter()
            .map(|i| SphereInstance { center: coords[i], radius: style::MARKER_RADIUS, color: style::SELECTION })
            .collect();
        self.highlight = upload_instances(device, "selection markers", &markers);
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
        let (cartoon, spheres, sticks) = match (self.representation, &self.cartoon) {
            (Representation::Spheres, _) => (None, self.all_spheres.as_ref(), None),
            (Representation::Sticks, _) | (_, None) => (None, None, Some(&self.all_sticks)),
            (Representation::Cartoon, Some(c)) => (Some(c), None, None),
            (Representation::CartoonAndLigands, Some(c)) => (Some(c), None, Some(&self.ligand_sticks)),
        };
        if let Some(mesh) = cartoon {
            pass.set_pipeline(&self.mesh_pipeline);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.count, 0, 0..1);
        }
        let joints = sticks.and_then(|s| s.joints.as_ref());
        for spheres in [spheres, joints].into_iter().flatten() {
            pass.set_pipeline(&self.sphere_pipeline);
            pass.set_vertex_buffer(0, spheres.buffer.slice(..));
            pass.draw(0..4, 0..spheres.count);
        }
        if let Some(bonds) = sticks.and_then(|s| s.bonds.as_ref()) {
            pass.set_pipeline(&self.stick_pipeline);
            pass.set_vertex_buffer(0, bonds.buffer.slice(..));
            pass.draw(0..4, 0..bonds.count);
        }
        // Last, so markers stay visible inside cartoons and behind other atoms.
        if let Some(markers) = &self.highlight {
            pass.set_pipeline(&self.highlight_pipeline);
            pass.set_vertex_buffer(0, markers.buffer.slice(..));
            pass.draw(0..4, 0..markers.count);
        }
    }
}

fn upload_sticks(device: &wgpu::Device, label: &str, (joints, bonds): (Vec<SphereInstance>, Vec<StickInstance>)) -> StickSet {
    StickSet {
        joints: upload_instances(device, &format!("{label} joints"), &joints),
        bonds: upload_instances(device, &format!("{label} bonds"), &bonds),
    }
}

fn upload_instances<T: Pod>(device: &wgpu::Device, label: &str, instances: &[T]) -> Option<Instances> {
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
