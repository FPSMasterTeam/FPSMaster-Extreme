// Debug visualization for the motion-vector buffer: maps the RG velocity (in uv
// units) to colour and blits it over the swapchain. Velocities are tiny, so they
// are amplified before encoding. Enabled by RECRAFT_MV / set_motion_vector_debug.
//   R = +x motion, G = +y motion, around a 0.5 grey rest. Static scene => flat
//   grey; panning the camera tints the screen toward the motion direction.

@group(0) @binding(0) var mv_tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

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
    let mv = textureSampleLevel(mv_tex, samp, in.uv, 0.0).rg * 40.0;
    return vec4<f32>(clamp(mv * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0)), 0.5, 1.0);
}
