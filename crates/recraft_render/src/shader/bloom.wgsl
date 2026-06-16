// Bloom in two cheap passes. The cost of a wide bloom is reading the full-res HDR
// scene many times; instead we downsample to an eighth-res target ONCE (a few
// full-res taps), then do the wide blur on that small, cache-friendly target. Far
// less full-res bandwidth than a wide blur sampling the full-res scene directly.

struct Params {
    // x: bloom threshold, y: bloom intensity, z: texel.x, w: texel.y
    p: vec4<f32>,
    q: vec4<f32>,
    r: vec4<f32>,
    s: vec4<f32>,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

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

// Pass 1: bright-pass + downsample the full-res scene to the eighth-res target with
// a 4-tap box (taps within the destination pixel's footprint to reduce aliasing).
@fragment
fn fs_bright(in: VsOut) -> @location(0) vec4<f32> {
    let o = params.p.zw * 4.0; // ~half an eighth-res texel, in full-res UV units
    var c = vec3<f32>(0.0);
    c += textureSampleLevel(src_tex, src_sampler, in.uv + vec2<f32>(-o.x, -o.y), 0.0).rgb;
    c += textureSampleLevel(src_tex, src_sampler, in.uv + vec2<f32>(o.x, -o.y), 0.0).rgb;
    c += textureSampleLevel(src_tex, src_sampler, in.uv + vec2<f32>(-o.x, o.y), 0.0).rgb;
    c += textureSampleLevel(src_tex, src_sampler, in.uv + vec2<f32>(o.x, o.y), 0.0).rgb;
    c *= 0.25;
    let lum = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let bright = max(lum - params.p.x, 0.0);
    return vec4<f32>(c * (bright / max(lum, 0.0001)), 1.0);
}

// Pass 2: wide Gaussian on the small bloom texture (cheap — it reads the eighth-res
// target, not the full-res scene). textureDimensions gives this target's texel.
@fragment
fn fs_blur(in: VsOut) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(src_tex));
    var bloom = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var i = -3; i <= 3; i = i + 1) {
        for (var j = -3; j <= 3; j = j + 1) {
            let off = vec2<f32>(f32(i), f32(j));
            let w = exp(-dot(off, off) / 8.0);
            bloom += textureSampleLevel(src_tex, src_sampler, in.uv + off * texel, 0.0).rgb * w;
            wsum += w;
        }
    }
    return vec4<f32>(bloom / wsum, 1.0);
}
