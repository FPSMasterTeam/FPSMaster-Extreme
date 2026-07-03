// Composites the ray-traced diffuse sky lighting onto the scene. Drawn as a fullscreen
// pass with ADDITIVE blending, so the framebuffer becomes scene + albedo * irradiance.
// Runs on the offscreen world target before the temporal upscale, so DLSS/TAA see the
// already-lit, denoised image. (The chunk shader keeps a small flat ambient floor; this
// adds the directional, sky-coloured, occlusion-aware fill on top.)
@group(0) @binding(0) var irradiance_tex: texture_2d<f32>;
@group(0) @binding(1) var albedo_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    out.uv = vec2<f32>(x, y);
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let irradiance = textureSampleLevel(irradiance_tex, samp, in.uv, 0.0).rgb;
    let albedo = textureSampleLevel(albedo_tex, samp, in.uv, 0.0).rgb;
    return vec4<f32>(albedo * irradiance, 0.0);
}
