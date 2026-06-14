// Post pass: HDR off-screen scene -> tone-mapped sRGB swapchain. Optional bloom
// (a bright-pass Gaussian disk sampled in HDR), exposure, ACES filmic tone map,
// then a light saturation / contrast grade. Also upscales for render scale < 1
// via the linear sampler. The colour target is sRGB, so the encode happens on
// write — this shader outputs linear [0,1].

struct Params {
    // x: bloom threshold, y: bloom intensity, z: texel.x, w: texel.y
    p: vec4<f32>,
    // x: exposure, y: saturation, z: contrast, w: bloom enabled (>0.5)
    q: vec4<f32>,
};

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen triangle.
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    out.uv = vec2<f32>(x, y);
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

// Narkowicz ACES filmic curve, scalar form.
fn aces_scalar(x: f32) -> f32 {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), 0.0, 1.0);
}

// Tone-map on luminance only and rescale the colour, so hue and saturation are
// preserved (no per-channel ACES hue shift / highlight desaturation). Keeps the
// filmic highlight roll-off without the colour cast.
fn tonemap(c: vec3<f32>) -> vec3<f32> {
    let l = max(dot(c, vec3<f32>(0.2126, 0.7152, 0.0722)), 1e-4);
    return c * (aces_scalar(l) / l);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var color = textureSample(scene_tex, scene_sampler, in.uv).rgb;

    if (params.q.w > 0.5) {
        // Bright-pass bloom: a 7x7 Gaussian disk, keeping only the energy above
        // the threshold (meaningful now that the scene is HDR).
        let texel = params.p.zw;
        var bloom = vec3<f32>(0.0);
        var wsum = 0.0;
        for (var i = -3; i <= 3; i = i + 1) {
            for (var j = -3; j <= 3; j = j + 1) {
                let o = vec2<f32>(f32(i), f32(j));
                let w = exp(-dot(o, o) / 8.0);
                let s = textureSample(scene_tex, scene_sampler, in.uv + o * texel).rgb;
                let lum = dot(s, vec3<f32>(0.2126, 0.7152, 0.0722));
                let bright = max(lum - params.p.x, 0.0);
                bloom += s * (bright / max(lum, 0.0001)) * w;
                wsum += w;
            }
        }
        color += bloom / wsum * params.p.y;
    }

    color = color * params.q.x;        // exposure
    color = tonemap(color);            // filmic tone map (HDR -> linear [0,1])

    // Saturation around luma.
    let l = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    color = mix(vec3<f32>(l), color, params.q.y);
    // Contrast around mid-grey.
    color = clamp((color - 0.5) * params.q.z + 0.5, vec3<f32>(0.0), vec3<f32>(1.0));

    return vec4<f32>(color, 1.0);
}
