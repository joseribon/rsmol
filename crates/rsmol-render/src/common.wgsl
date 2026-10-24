// Shared by every pipeline: camera uniforms and the lighting model.

struct Uniforms {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;

// Colors arrive as sRGB bytes; lighting happens in linear space.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3(2.2));
}

// Headlight key light plus a soft fill from above, Blinn-Phong specular and a rim term that
// darkens silhouettes to give a cheap sense of depth until SSAO lands.
// `n` is the view-space normal, `v` the unit vector from the surface towards the eye.
fn shade(albedo: vec3<f32>, n: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    let key = normalize(vec3(0.3, 0.4, 1.0));
    let fill = normalize(vec3(-0.5, 1.0, 0.2));
    let diffuse = max(dot(n, key), 0.0) * 0.75 + max(dot(n, fill), 0.0) * 0.2;
    let spec = pow(max(dot(n, normalize(key + v)), 0.0), 48.0) * 0.35;
    let rim = mix(0.55, 1.0, smoothstep(0.0, 0.6, dot(n, v)));
    return (albedo * (0.18 + diffuse) + vec3(spec)) * rim;
}
