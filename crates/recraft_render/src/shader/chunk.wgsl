struct Camera {
    view_proj: mat4x4<f32>,
    // Day/night sky-light scale (vanilla getSunBrightness): 1.0 by day, ~0.2 at
    // night. Applied to each vertex's sky-light term so open ground darkens at
    // night while block-lit (torch/lava) surfaces keep their brightness.
    sky_brightness: f32,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(1) @binding(0)
var block_atlas: texture_2d<f32>;

@group(1) @binding(1)
var block_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
    // (sky_light, block_light) brightness curves, combined per-fragment with the
    // day/night factor. (0, 1) means full-bright (items/GUI/break overlay).
    @location(3) light: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) light: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(input.position, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    out.light = input.light;
    return out;
}

// Combine the sky/block light curves with the time-of-day sky factor. Sky light
// is scaled by `sky_brightness`; block light is not, so torches stay lit at
// night. `max` matches vanilla's lightmap (the brighter source wins).
fn day_night(light: vec2<f32>) -> f32 {
    return max(light.x * camera.sky_brightness, light.y);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    let b = day_night(input.light);
    return vec4<f32>(texel.rgb * input.color.rgb * b, texel.a * input.color.a);
}

// Cutout pass: respect the texture's alpha channel (e.g. leaf gaps) by
// discarding near-transparent texels, keeping the rest fully opaque.
@fragment
fn fs_cutout(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    if (texel.a * input.color.a < 0.5) {
        discard;
    }
    let b = day_night(input.light);
    return vec4<f32>(texel.rgb * input.color.rgb * b, 1.0);
}
