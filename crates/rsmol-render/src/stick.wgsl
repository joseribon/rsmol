// Ray-cast cylinder impostors for bonds: one camera-facing quad per bond. Each half of the bond
// takes the color of its atom; the round joints are drawn as spheres by the sphere pipeline.
// Prepended with common.wgsl.

struct Instance {
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
    @location(2) radius: f32,
    @location(3) color_a: vec4<f32>,
    @location(4) color_b: vec4<f32>,
    /// Atom index + 1 of each end, for picking.
    @location(5) id_a: u32,
    @location(6) id_b: u32,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) view_pos: vec3<f32>,
    @location(1) @interpolate(flat) a: vec3<f32>,
    @location(2) @interpolate(flat) b: vec3<f32>,
    @location(3) @interpolate(flat) radius: f32,
    @location(4) @interpolate(flat) color_a: vec3<f32>,
    @location(5) @interpolate(flat) color_b: vec3<f32>,
    @location(6) @interpolate(flat) id_a: u32,
    @location(7) @interpolate(flat) id_b: u32,
}

// Margin so the quad covers the silhouette under perspective.
const QUAD_SCALE: f32 = 1.25;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    let a = (u.view * vec4(inst.a, 1.0)).xyz;
    let b = (u.view * vec4(inst.b, 1.0)).xyz;
    let r = inst.radius;
    let axis = normalize(b - a);
    let mid = (a + b) * 0.5;
    let to_eye = normalize(-mid);
    // Perpendicular to both the bond and the view ray; degenerate when looking down the bond.
    var side = cross(axis, to_eye);
    if dot(side, side) < 1e-6 {
        side = cross(axis, vec3(0.0, 1.0, 0.0));
    }
    side = normalize(side) * r * QUAD_SCALE;

    // Strip order: (a, -side), (a, +side), (b, -side), (b, +side); extended by r past each end
    // and pulled towards the eye so the cylinder's front surface is never clipped.
    let end = select(a - axis * r, b + axis * r, vi >= 2u);
    let sign = select(-1.0, 1.0, (vi & 1u) == 1u);
    let view_pos = end + side * sign + to_eye * r;

    var out: VsOut;
    out.clip = u.proj * vec4(view_pos, 1.0);
    out.view_pos = view_pos;
    out.a = a;
    out.b = b;
    out.radius = r;
    out.color_a = srgb_to_linear(inst.color_a.rgb);
    out.color_b = srgb_to_linear(inst.color_b.rgb);
    out.id_a = inst.id_a;
    out.id_b = inst.id_b;
    return out;
}

struct FsOut {
    @location(0) color: vec4<f32>,
    @location(1) id: u32,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fs_main(in: VsOut) -> FsOut {
    // Ray from the eye (origin) against the finite cylinder a→b (Quílez's formulation).
    let rd = normalize(in.view_pos);
    let ba = in.b - in.a;
    let oc = -in.a;
    let baba = dot(ba, ba);
    let bard = dot(ba, rd);
    let baoc = dot(ba, oc);
    let k2 = baba - bard * bard;
    let k1 = baba * dot(oc, rd) - baoc * bard;
    let k0 = baba * dot(oc, oc) - baoc * baoc - in.radius * in.radius * baba;
    let h = k1 * k1 - k2 * k0;
    if h < 0.0 || k2 < 1e-8 {
        discard;
    }
    let t = (-k1 - sqrt(h)) / k2;
    let y = baoc + t * bard;
    // Outside the segment: the end caps are covered by the joint spheres.
    if y < 0.0 || y > baba {
        discard;
    }
    let hit = rd * t;
    let n = (oc + rd * t - ba * (y / baba)) / in.radius;
    let near_a = y < baba * 0.5;
    let albedo = select(in.color_b, in.color_a, near_a);

    let clip = u.proj * vec4(hit, 1.0);
    var out: FsOut;
    out.color = vec4(shade(albedo, n, -rd), 1.0);
    out.id = select(in.id_b, in.id_a, near_a);
    out.depth = clip.z / clip.w;
    return out;
}
