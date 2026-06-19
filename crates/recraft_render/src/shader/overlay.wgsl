struct Camera {
    view_proj: mat4x4<f32>,
    sky_brightness: f32,
    time: f32,
    tile_size: vec2<f32>,
    // Render origin in whole blocks (camera-relative rendering); GUI binds origin 0.
    origin: vec4<i32>,
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
    let rel = input.position - vec3<f32>(camera.origin.xyz);
    out.clip_position = camera.view_proj * vec4<f32>(rel, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    out.light = input.light;
    return out;
}

fn light_level(l: f32) -> f32 {
    return l / (4.0 - 3.0 * clamp(l, 0.0, 1.0));
}

fn day_night(light: vec2<f32>) -> f32 {
    return max(light_level(light.x) * camera.sky_brightness, light_level(light.y));
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    let b = day_night(input.light);
    return vec4<f32>(texel.rgb * input.color.rgb * b, texel.a * input.color.a);
}

@fragment
fn fs_cutout(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    if (texel.a * input.color.a < 0.5) {
        discard;
    }
    let b = day_night(input.light);
    return vec4<f32>(texel.rgb * input.color.rgb * b, 1.0);
}
