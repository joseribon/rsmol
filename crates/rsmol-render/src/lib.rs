//! wgpu renderer for rsmol.
//!
//! Window-system agnostic: the host (winit viewer, Tauri shell, browser canvas) owns the surface
//! and hands the renderer a texture view to draw into each frame.
//!
//! A frame has three passes: the scene (cartoon, spheres, sticks) into an HDR color target and
//! depth buffer; a full-screen composite adding ambient occlusion, outlines and fog; and the
//! selection markers on top.

pub mod atoms;
mod camera;
pub mod cartoon;
pub mod style;

use std::borrow::Cow;

use atoms::{SphereInstance, StickInstance};
use bytemuck::{Pod, Zeroable};
use rsmol_core::backbone::{self, Trace};
use rsmol_core::display::{Display, Reps};
use rsmol_core::{Structure, Vec3};
use wgpu::util::DeviceExt;

pub use camera::Camera;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Linear HDR scene target, so lighting is not quantized before the composite.
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Look of the composite pass. Colors are linear RGB.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub background_top: [f32; 3],
    pub background_bottom: [f32; 3],
    pub outline: [f32; 4],
    /// Smallest ambient-occlusion radius in Å; it grows with viewing distance so overviews
    /// get soft, large-scale shadows and close-ups keep fine contact detail.
    pub ao_radius: f32,
    /// Ambient-occlusion radius as a fraction of the camera distance.
    pub ao_radius_per_distance: f32,
    /// How dark full occlusion gets.
    pub ao_strength: f32,
    /// Silhouette outline offset in pixels (2 suits Retina displays).
    pub outline_px: f32,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            background_top: [0.016, 0.019, 0.026],
            background_bottom: [0.004, 0.005, 0.008],
            outline: [0.0, 0.0, 0.0, 0.85],
            ao_radius: 4.0,
            ao_radius_per_distance: 0.04,
            ao_strength: 1.8,
            outline_px: 2.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneUniforms {
    view: [[f32; 4]; 4],
    proj: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostUniforms {
    inv_proj: [[f32; 4]; 4],
    bg_top: [f32; 4],
    bg_bottom: [f32; 4],
    outline: [f32; 4],
    resolution: [f32; 2],
    proj_scale: f32,
    ao_radius: f32,
    ao_strength: f32,
    outline_px: f32,
    fog_near: f32,
    fog_far: f32,
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

/// Render targets that depend on the viewport size.
struct Targets {
    scene: wgpu::TextureView,
    depth: wgpu::TextureView,
    post_bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

pub struct Renderer {
    sphere_pipeline: wgpu::RenderPipeline,
    stick_pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    marker_pipeline: wgpu::RenderPipeline,
    post_pipeline: wgpu::RenderPipeline,
    post_layout: wgpu::BindGroupLayout,
    scene_uniforms: wgpu::Buffer,
    post_uniforms: wgpu::Buffer,
    scene_bind_group: wgpu::BindGroup,
    spheres: Option<Instances>,
    joints: Option<Instances>,
    sticks: Option<Instances>,
    cartoon: Option<GpuMesh>,
    markers: Option<Instances>,
    targets: Targets,
    pub style: Style,
}

fn shader(device: &wgpu::Device, label: &str, source: &'static str, common: bool) -> wgpu::ShaderModule {
    let full = if common { format!("{}\n{source}", include_str!("common.wgsl")) } else { source.to_owned() };
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(full)),
    })
}

const SPHERE_LAYOUT: wgpu::VertexBufferLayout = wgpu::VertexBufferLayout {
    array_stride: size_of::<SphereInstance>() as u64,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Unorm8x4],
};

enum Kind {
    /// Depth-tested geometry into the HDR scene target.
    Scene,
    /// Alpha-blended over the final image, without depth.
    Overlay,
    /// The full-screen composite.
    Post,
}

impl Renderer {
    /// `output_format` is the format of the views later passed to `render`.
    pub fn new(device: &wgpu::Device, output_format: wgpu::TextureFormat, width: u32, height: u32) -> Self {
        let uniform_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let buffer = |label, size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let scene_uniforms = buffer("scene uniforms", size_of::<SceneUniforms>() as u64);
        let post_uniforms = buffer("post uniforms", size_of::<PostUniforms>() as u64);

        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &[uniform_entry(0)],
        });
        let scene_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout: &scene_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: scene_uniforms.as_entire_binding() }],
        });
        let texture_entry = |binding, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[
                uniform_entry(0),
                texture_entry(1, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(2, wgpu::TextureSampleType::Depth),
            ],
        });
        let layout = |bind: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(bind)],
                immediate_size: 0,
            })
        };
        let (scene_pipeline_layout, post_pipeline_layout) = (layout(&scene_layout), layout(&post_layout));

        let pipeline = |label: &str,
                        module: &wgpu::ShaderModule,
                        buffer: Option<wgpu::VertexBufferLayout>,
                        topology: wgpu::PrimitiveTopology,
                        kind: Kind| {
            let constants: &[(&str, f64)] = if matches!(kind, Kind::Overlay) { &[("ALPHA", 0.55)] } else { &[] };
            let options = wgpu::PipelineCompilationOptions { constants, ..Default::default() };
            let (format, blend) = match kind {
                Kind::Scene => (SCENE_FORMAT, None),
                Kind::Overlay => (output_format, Some(wgpu::BlendState::ALPHA_BLENDING)),
                Kind::Post => (output_format, None),
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(if matches!(kind, Kind::Post) { &post_pipeline_layout } else { &scene_pipeline_layout }),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    compilation_options: options.clone(),
                    buffers: &[buffer],
                },
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: matches!(kind, Kind::Scene).then(|| wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(if matches!(kind, Kind::Overlay) { "fs_marker" } else { "fs_main" }),
                    compilation_options: options,
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        use wgpu::PrimitiveTopology::{TriangleList, TriangleStrip};
        let sphere_shader = shader(device, "spheres", include_str!("sphere.wgsl"), true);
        let sphere_pipeline = pipeline("spheres", &sphere_shader, Some(SPHERE_LAYOUT), TriangleStrip, Kind::Scene);
        let marker_pipeline = pipeline("selection markers", &sphere_shader, Some(SPHERE_LAYOUT), TriangleStrip, Kind::Overlay);
        let stick_pipeline = pipeline(
            "sticks",
            &shader(device, "sticks", include_str!("stick.wgsl"), true),
            Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<StickInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x3, 1 => Float32x3, 2 => Float32, 3 => Unorm8x4, 4 => Unorm8x4
                ],
            }),
            TriangleStrip,
            Kind::Scene,
        );
        let mesh_pipeline = pipeline(
            "mesh",
            &shader(device, "mesh", include_str!("mesh.wgsl"), true),
            Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<cartoon::MeshVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Unorm8x4],
            }),
            TriangleList,
            Kind::Scene,
        );
        let post_shader = shader(device, "composite", include_str!("post.wgsl"), false);
        let post_pipeline = pipeline("composite", &post_shader, None, TriangleList, Kind::Post);

        let targets = create_targets(device, &post_layout, &post_uniforms, width, height);
        Renderer {
            sphere_pipeline,
            stick_pipeline,
            mesh_pipeline,
            marker_pipeline,
            post_pipeline,
            post_layout,
            scene_uniforms,
            post_uniforms,
            scene_bind_group,
            spheres: None,
            joints: None,
            sticks: None,
            cartoon: None,
            markers: None,
            targets,
            style: Style::default(),
        }
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if (width, height) != self.targets.size && width > 0 && height > 0 {
            self.targets = create_targets(device, &self.post_layout, &self.post_uniforms, width, height);
        }
    }

    /// Rebuilds all geometry for `structure` as `display` describes it.
    pub fn set_scene(&mut self, device: &wgpu::Device, structure: &Structure, display: &Display, state: usize) {
        self.spheres = None;
        self.joints = None;
        self.sticks = None;
        self.cartoon = None;
        let Some(coords) = structure.coords(state) else { return };

        // Cartoon for runs of residues whose guide atom is shown as cartoon. Sticks may attach
        // to those guide atoms, so side chains connect to the ribbon.
        let traces = shown_traces(structure, state, display);
        let mut anchors = vec![false; coords.len()];
        let mut residue_color = vec![[0u8; 4]; structure.residues.len()];
        for t in &traces {
            for (&r, &a) in t.residues.iter().zip(&t.guide_atoms) {
                anchors[a as usize] = true;
                residue_color[r as usize] = display.colors[a as usize];
            }
        }
        let mesh = cartoon::build(structure, state, &traces, |r| residue_color[r as usize]);
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

        self.spheres = upload(device, "spheres", &atoms::spheres(structure, coords, display));
        let (joints, sticks) = atoms::sticks(structure, coords, display, &anchors);
        self.joints = upload(device, "stick joints", &joints);
        self.sticks = upload(device, "sticks", &sticks);
    }

    /// Marks `atoms` as selected; an empty iterator clears the markers.
    pub fn set_highlight(&mut self, device: &wgpu::Device, coords: &[Vec3], atoms: impl IntoIterator<Item = usize>) {
        let markers: Vec<SphereInstance> = atoms
            .into_iter()
            .map(|i| SphereInstance { center: coords[i], radius: style::MARKER_RADIUS, color: style::SELECTION })
            .collect();
        self.markers = upload(device, "selection markers", &markers);
    }

    pub fn render(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, camera: &Camera) {
        let (width, height) = self.targets.size;
        let proj = camera.proj(width as f32 / height as f32);
        let scene = SceneUniforms { view: camera.view().to_cols_array_2d(), proj: proj.to_cols_array_2d() };
        queue.write_buffer(&self.scene_uniforms, 0, bytemuck::bytes_of(&scene));
        let s = self.style;
        let (fog_near, fog_far) = camera.depth_range();
        let rgba = |c: [f32; 3]| [c[0], c[1], c[2], 1.0];
        let post = PostUniforms {
            inv_proj: proj.inverse().to_cols_array_2d(),
            bg_top: rgba(s.background_top),
            bg_bottom: rgba(s.background_bottom),
            outline: s.outline,
            resolution: [width as f32, height as f32],
            proj_scale: proj.y_axis.y,
            ao_radius: s.ao_radius.max(camera.distance * s.ao_radius_per_distance),
            ao_strength: s.ao_strength,
            outline_px: s.outline_px,
            fog_near,
            fog_far,
        };
        queue.write_buffer(&self.post_uniforms, 0, bytemuck::bytes_of(&post));

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.scene,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.targets.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.scene_bind_group, &[]);
            if let Some(mesh) = &self.cartoon {
                pass.set_pipeline(&self.mesh_pipeline);
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.count, 0, 0..1);
            }
            for spheres in [&self.spheres, &self.joints].into_iter().flatten() {
                pass.set_pipeline(&self.sphere_pipeline);
                pass.set_vertex_buffer(0, spheres.buffer.slice(..));
                pass.draw(0..4, 0..spheres.count);
            }
            if let Some(sticks) = &self.sticks {
                pass.set_pipeline(&self.stick_pipeline);
                pass.set_vertex_buffer(0, sticks.buffer.slice(..));
                pass.draw(0..4, 0..sticks.count);
            }
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.post_pipeline);
        pass.set_bind_group(0, &self.targets.post_bind_group, &[]);
        pass.draw(0..3, 0..1);
        // Markers go on top of the composite so occlusion and fog never hide a selection.
        if let Some(markers) = &self.markers {
            pass.set_pipeline(&self.marker_pipeline);
            pass.set_bind_group(0, &self.scene_bind_group, &[]);
            pass.set_vertex_buffer(0, markers.buffer.slice(..));
            pass.draw(0..4, 0..markers.count);
        }
    }
}

/// Backbone traces cut down to runs of at least two residues shown as cartoon.
fn shown_traces(s: &Structure, state: usize, display: &Display) -> Vec<Trace> {
    let mut out = Vec::new();
    for t in backbone::traces(s, state) {
        let shown = |k: usize| display.reps[t.guide_atoms[k] as usize].contains(Reps::CARTOON);
        let mut k = 0;
        while k < t.len() {
            let start = k;
            while k < t.len() && shown(k) {
                k += 1;
            }
            if k - start >= 2 {
                out.push(Trace {
                    kind: t.kind,
                    chain: t.chain,
                    residues: t.residues[start..k].to_vec(),
                    guide_atoms: t.guide_atoms[start..k].to_vec(),
                    orient_atoms: t.orient_atoms[start..k].to_vec(),
                });
            }
            k += 1;
        }
    }
    out
}

fn create_targets(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, uniforms: &wgpu::Buffer, width: u32, height: u32) -> Targets {
    let texture = |label, format| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    let scene = texture("scene color", SCENE_FORMAT);
    let depth = texture("depth", DEPTH_FORMAT);
    let post_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("post"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&scene) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&depth) },
        ],
    });
    Targets { scene, depth, post_bind_group, size: (width.max(1), height.max(1)) }
}

fn upload<T: Pod>(device: &wgpu::Device, label: &str, items: &[T]) -> Option<Instances> {
    (!items.is_empty()).then(|| Instances {
        buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(items),
            usage: wgpu::BufferUsages::VERTEX,
        }),
        count: items.len() as u32,
    })
}
