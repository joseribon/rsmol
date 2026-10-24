// Ray-cast sphere impostors: one camera-facing quad per atom, with the exact sphere surface and
// depth computed per fragment. Prepended with common.wgsl.

struct Instance {
    @location(0) center: vec3<f32>,
    @location(1) radius: f32,
    @location(2) color: vec4<f32>,
    /// Atom index + 1 for picking; 0 means "nothing".
    @location(3) id: u32,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    /// View-space position of this quad corner; the eye is at the origin.
    @location(0) view_pos: vec3<f32>,
    @location(1) @interpolate(flat) center: vec3<f32>,
    @location(2) @interpolate(flat) radius: f32,
    @location(3) @interpolate(flat) color: vec3<f32>,
    @location(4) @interpolate(flat) id: u32,
}

// Quad corners for a 4-vertex triangle strip.
const CORNERS = array<vec2<f32>, 4>(vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0), vec2(1.0, 1.0));

// Overridden to < 1 by the selection-marker pipeline, which blends over the scene.
override ALPHA: f32 = 1.0;

// Margin so the quad still covers the silhouette of off-axis spheres under perspective.
const QUAD_SCALE: f32 = 1.15;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    let center = (u.view * vec4(inst.center, 1.0)).xyz;
    // Place the quad on the sphere's front face so it is never clipped by the sphere itself.
    let front = center + normalize(-center) * inst.radius;
    let view_pos = front + vec3(CORNERS[vi] * inst.radius * QUAD_SCALE, 0.0);

    var out: VsOut;
    out.clip = u.proj * vec4(view_pos, 1.0);
    out.view_pos = view_pos;
    out.center = center;
    out.radius = inst.radius;
    out.color = srgb_to_linear(inst.color.rgb);
    out.id = inst.id;
    return out;
}

struct FsOut {
    @location(0) color: vec4<f32>,
    @location(1) id: u32,
    @builtin(frag_depth) depth: f32,
}

struct Hit {
    pos: vec3<f32>,
    normal: vec3<f32>,
}

fn intersect(in: VsOut) -> Hit {
    let dir = normalize(in.view_pos);
    let b = dot(dir, in.center);
    let disc = b * b - (dot(in.center, in.center) - in.radius * in.radius);
    if disc < 0.0 {
        discard;
    }
    let hit = dir * (b - sqrt(disc));
    return Hit(hit, (hit - in.center) / in.radius);
}

@fragment
fn fs_main(in: VsOut) -> FsOut {
    let hit = intersect(in);
    let clip = u.proj * vec4(hit.pos, 1.0);
    var out: FsOut;
    out.color = vec4(shade(in.color, hit.normal, -normalize(hit.pos)), 1.0);
    out.id = in.id;
    out.depth = clip.z / clip.w;
    return out;
}

// Selection markers: blended over the final image, so no depth is written.
@fragment
fn fs_marker(in: VsOut) -> @location(0) vec4<f32> {
    let hit = intersect(in);
    return vec4(shade(in.color, hit.normal, -normalize(hit.pos)), ALPHA);
}
