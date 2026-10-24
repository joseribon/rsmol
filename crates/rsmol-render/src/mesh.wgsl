// Lit triangle meshes (cartoon). Prepended with common.wgsl.

struct Vertex {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) view_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
}

@vertex
fn vs_main(v: Vertex) -> VsOut {
    let view_pos = (u.view * vec4(v.position, 1.0)).xyz;
    var out: VsOut;
    out.clip = u.proj * vec4(view_pos, 1.0);
    out.view_pos = view_pos;
    // The view matrix is a rigid transform, so it maps normals correctly.
    out.normal = (u.view * vec4(v.normal, 0.0)).xyz;
    out.color = srgb_to_linear(v.color.rgb);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let v = normalize(-in.view_pos);
    var n = normalize(in.normal);
    // Light whichever side faces the camera, independent of triangle winding.
    if dot(n, v) < 0.0 {
        n = -n;
    }
    return vec4(shade(in.color, n, v), 1.0);
}
