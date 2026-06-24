// Composites the denoised AO onto the HDR scene. Drawn as a fullscreen pass with
// multiply blending (src_factor = Dst, dst_factor = Zero), so the framebuffer becomes
// scene_rgb * ao while alpha is preserved. Runs on the offscreen world target before
// the temporal upscale, so DLSS/TAA see an already-occluded, noise-free image.
@group(0) @binding(0) var ao_tex: texture_2d<f32>;
@group(0) @binding(1) var ao_samp: sampler;

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
    let ao = textureSampleLevel(ao_tex, ao_samp, in.uv, 0.0).r;
    return vec4<f32>(ao, ao, ao, 1.0);
}
