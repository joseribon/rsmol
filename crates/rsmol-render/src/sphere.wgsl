// Ray-cast sphere impostors: one camera-facing quad per atom, with the exact sphere surface and
// depth computed per fragment.

struct Uniforms {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;

struct Instance {
    @location(0) center: vec3<f32>,
    @location(1) radius: f32,
    @location(2) color: vec4<f32>,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    /// View-space position of this quad corner; the eye is at the origin.
    @location(0) view_pos: vec3<f32>,
    @location(1) @interpolate(flat) center: vec3<f32>,
    @location(2) @interpolate(flat) radius: f32,
    @location(3) @interpolate(flat) color: vec3<f32>,
}

// Quad corners for a 4-vertex triangle strip.
const CORNERS = array<vec2<f32>, 4>(vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0), vec2(1.0, 1.0));

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
    // Colors arrive as sRGB bytes; lighting happens in linear space.
    out.color = pow(inst.color.rgb, vec3(2.2));
    return out;
}

struct FsOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fs_main(in: VsOut) -> FsOut {
    let dir = normalize(in.view_pos);
    let b = dot(dir, in.center);
    let disc = b * b - (dot(in.center, in.center) - in.radius * in.radius);
    if disc < 0.0 {
        discard;
    }
    let hit = dir * (b - sqrt(disc));
    let n = (hit - in.center) / in.radius;

    // Headlight key light plus a soft fill from above, Blinn-Phong specular and a rim term that
    // darkens silhouettes to give a cheap sense of depth until SSAO lands.
    let v = -dir;
    let key = normalize(vec3(0.3, 0.4, 1.0));
    let fill = normalize(vec3(-0.5, 1.0, 0.2));
    let diffuse = max(dot(n, key), 0.0) * 0.75 + max(dot(n, fill), 0.0) * 0.2;
    let spec = pow(max(dot(n, normalize(key + v)), 0.0), 48.0) * 0.35;
    let rim = mix(0.55, 1.0, smoothstep(0.0, 0.6, dot(n, v)));
    let lit = (in.color * (0.18 + diffuse) + vec3(spec)) * rim;

    let clip = u.proj * vec4(hit, 1.0);
    var out: FsOut;
    out.color = vec4(lit, 1.0);
    out.depth = clip.z / clip.w;
    return out;
}
