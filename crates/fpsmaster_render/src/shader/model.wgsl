struct Camera {
    view_proj: mat4x4<f32>,
    sky_brightness: f32,
    time: f32,
    tile_size: vec2<f32>,
    // Render origin in whole blocks (camera-relative rendering). World pipelines
    // bind a camera with the real origin; the GUI binds one with origin 0, so its
    // clip-space-baked vertices pass through unchanged.
    origin: vec4<i32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(1) @binding(0)
var entity_texture: texture_2d<f32>;
@group(1) @binding(1)
var entity_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let rel = input.position - vec3<f32>(camera.origin.xyz);
    out.clip_position = camera.view_proj * vec4<f32>(rel, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    return out;
}

// sRGB -> linear; the same decode `chunk.wgsl` and `ui.wgsl` apply. The entity
// atlas is `Rgba8UnormSrgb`, so `textureSample` returns linear, while the vertex
// colour carries vanilla gamma-space multipliers (the per-face model shade and
// the entity's lightmap). Multiplying them in linear would raise each to ~1/2.2,
// flattening entity shading exactly as it flattened the terrain.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(entity_texture, entity_sampler, input.uv);
    let result = vec4<f32>(
        texel.rgb * srgb_to_linear(input.color.rgb),
        texel.a * input.color.a,
    );
    // Discard fully transparent texels so skin overlay gaps don't draw.
    if (result.a < 0.01) {
        discard;
    }
    return result;
}
