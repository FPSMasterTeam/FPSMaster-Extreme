// Single-pass bloom composite: replaces the upscale blit when bloom is on.
// Samples the off-screen scene in a Gaussian disk, keeps only the bright part of
// each tap, and adds that glow back over the scene — sun disc, glowing blocks and
// bright highlights bleed light. Cheap enough for the dev GPU; optional on weak.

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let xy = corners[vi];
    var out: VsOut;
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>((xy.x + 1.0) * 0.5, (1.0 - xy.y) * 0.5);
    return out;
}

struct Params {
    // x = threshold, y = intensity, z = texel.x, w = texel.y
    p: vec4<f32>,
};

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

fn bright(c: vec3<f32>, threshold: f32) -> vec3<f32> {
    let lum = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    return c * smoothstep(threshold, threshold + 0.25, lum);
}

@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4<f32> {
    let scene = textureSample(scene_tex, scene_sampler, in.uv).rgb;
    let texel = params.p.zw;
    let spread = 2.0; // pixels between taps → ~12px glow radius
    var bloom = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var dy = -3; dy <= 3; dy = dy + 1) {
        for (var dx = -3; dx <= 3; dx = dx + 1) {
            let fd = vec2<f32>(f32(dx), f32(dy));
            let w = exp(-(fd.x * fd.x + fd.y * fd.y) / 8.0);
            let off = fd * texel * spread;
            bloom = bloom + bright(textureSample(scene_tex, scene_sampler, in.uv + off).rgb, params.p.x) * w;
            wsum = wsum + w;
        }
    }
    bloom = bloom / wsum;
    return vec4<f32>(scene + bloom * params.p.y, 1.0);
}
