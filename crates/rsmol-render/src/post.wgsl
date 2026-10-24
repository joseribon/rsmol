// Full-screen composite: ambient occlusion from depth, silhouette outlines, depth fog and a
// gradient background, applied to the lit scene.

struct Post {
    inv_proj: mat4x4<f32>,
    bg_top: vec4<f32>,
    bg_bottom: vec4<f32>,
    outline: vec4<f32>,
    resolution: vec2<f32>,
    /// proj[1][1]: converts view-space lengths to NDC at unit depth.
    proj_scale: f32,
    ao_radius: f32,
    ao_strength: f32,
    outline_px: f32,
    fog_near: f32,
    fog_far: f32,
}

@group(0) @binding(0) var<uniform> p: Post;
@group(0) @binding(1) var scene: texture_2d<f32>;
@group(0) @binding(2) var depth: texture_depth_2d;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the screen.
    let uv = vec2(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn clamp_px(px: vec2<i32>) -> vec2<i32> {
    return clamp(px, vec2(0), vec2<i32>(p.resolution) - 1);
}

fn depth_at(px: vec2<i32>) -> f32 {
    return textureLoad(depth, clamp_px(px), 0);
}

fn view_pos(px: vec2<i32>) -> vec3<f32> {
    let q = clamp_px(px);
    let uv = (vec2<f32>(q) + 0.5) / p.resolution;
    let ndc = vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, textureLoad(depth, q, 0), 1.0);
    let v = p.inv_proj * ndc;
    return v.xyz / v.w;
}

// Interleaved gradient noise (Jimenez 2014): decorrelates sample rotations between pixels.
fn noise(px: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(px, vec2(0.06711056, 0.00583715))));
}

fn ambient_occlusion(px: vec2<i32>, pos: vec3<f32>) -> f32 {
    // Normal from the neighbor on the side with the smaller depth step, so edges stay sharp.
    let r = view_pos(px + vec2(1, 0)) - pos;
    let l = pos - view_pos(px - vec2(1, 0));
    let d = view_pos(px + vec2(0, 1)) - pos;
    let u = pos - view_pos(px - vec2(0, 1));
    let dx = select(l, r, abs(r.z) < abs(l.z));
    let dy = select(u, d, abs(d.z) < abs(u.z));
    var n = normalize(cross(dy, dx));
    if dot(n, -pos) < 0.0 {
        n = -n;
    }

    let radius_px = clamp(p.ao_radius * p.proj_scale * 0.5 * p.resolution.y / -pos.z, 2.0, p.resolution.y * 0.1);
    let rotation = noise(vec2<f32>(px)) * 6.2831853;
    const SAMPLES: i32 = 16;
    var occlusion = 0.0;
    for (var i = 0; i < SAMPLES; i++) {
        // Golden-angle spiral, denser near the center.
        let t = (f32(i) + 0.5) / f32(SAMPLES);
        let angle = f32(i) * 2.3999632 + rotation;
        let offset = vec2(cos(angle), sin(angle)) * t * radius_px;
        let v = view_pos(px + vec2<i32>(offset)) - pos;
        let dist = length(v);
        let falloff = 1.0 - smoothstep(p.ao_radius * 0.5, p.ao_radius, dist);
        occlusion += max(dot(v / max(dist, 1e-4), n) - 0.15, 0.0) * falloff;
    }
    return clamp(1.0 - p.ao_strength * occlusion / f32(SAMPLES), 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(frag.xy);
    let uv_y = frag.y / p.resolution.y;
    let background = mix(p.bg_top.rgb, p.bg_bottom.rgb, uv_y);
    let d = depth_at(px);

    // Silhouette: a neighbor much closer than this pixel (or this pixel is background).
    let z = select(-view_pos(px).z, 1e9, d >= 1.0);
    let o = i32(p.outline_px);
    var steps = array<vec2<i32>, 4>(vec2(o, 0), vec2(-o, 0), vec2(0, o), vec2(0, -o));
    var nearest = z;
    for (var k = 0; k < 4; k++) {
        let step = steps[k];
        let dn = depth_at(px + step);
        if dn < 1.0 {
            nearest = min(nearest, -view_pos(px + step).z);
        }
    }
    let edge = z - nearest > max(1.0, nearest * 0.03);

    if d >= 1.0 {
        return vec4(select(background, mix(background, p.outline.rgb, p.outline.a), edge), 1.0);
    }

    let pos = view_pos(px);
    var color = textureLoad(scene, px, 0).rgb * ambient_occlusion(px, pos);
    let fog = clamp((-pos.z - p.fog_near) / max(p.fog_far - p.fog_near, 1e-3), 0.0, 1.0);
    color = mix(color, background, fog * fog * 0.7);
    if edge {
        color = mix(color, p.outline.rgb, p.outline.a);
    }
    return vec4(color, 1.0);
}
