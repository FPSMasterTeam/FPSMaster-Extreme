// Minimal world-space colored-line shader for the debug-overlay pipeline
// (extension blockOutline / chunkBorders / entityBox presets).

struct Camera {
    view_proj: mat4x4<f32>,
    sky_brightness: f32,
    time: f32,
    tile_size: vec2<f32>,
    // Render origin in whole blocks (camera-relative rendering); see chunk.wgsl.
    origin: vec4<i32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let rel = input.position - vec3<f32>(camera.origin.xyz);
    out.clip_position = camera.view_proj * vec4<f32>(rel, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
