// Rain/snow curtain. One tiled, scrolling quad per world column; the geometry
// and the vanilla port notes live in `weather.rs`. The sampler wraps, because
// the V coordinate is `blockY * 0.25 + scroll` and runs far outside 0..1.

struct Camera {
    view_proj: mat4x4<f32>,
    sky_brightness: f32,
    time: f32,
    tile_size: vec2<f32>,
    origin: vec4<i32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(1) @binding(0)
var weather_texture: texture_2d<f32>;
@group(1) @binding(1)
var weather_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) light: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) light: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // Camera-relative, like every other world pipeline.
    let rel = in.position - vec3<f32>(camera.origin.xyz);
    out.clip_position = camera.view_proj * vec4<f32>(rel, 1.0);
    out.color = in.color;
    out.uv = in.uv;
    out.light = in.light;
    return out;
}

fn light_level(l: f32) -> f32 {
    return l / (4.0 - 3.0 * clamp(l, 0.0, 1.0));
}

// See the long note in chunk.wgsl: the atlas is sRGB so the sample is linear,
// while the light level is a vanilla gamma-space multiplier.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(weather_texture, weather_sampler, in.uv);
    let alpha = texel.a * in.color.a;
    // The curtain textures are mostly empty; dropping the clear texels early
    // keeps the overdraw down at the radius where quads stack up.
    if (alpha < 0.01) {
        discard;
    }
    // Lit by the column's own light, as vanilla does with `getCombinedLight`,
    // so rain over a torch-lit doorway is brighter than rain over dark ground.
    let level = max(
        light_level(in.light.x) * camera.sky_brightness,
        light_level(in.light.y),
    );
    let lit = srgb_to_linear(vec3<f32>(level));
    return vec4<f32>(texel.rgb * in.color.rgb * lit, alpha);
}
