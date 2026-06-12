struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0)
var ui_texture: texture_2d<f32>;

@group(0) @binding(1)
var ui_sampler: sampler;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOut {
    var position = vec2<f32>(-1.0, -1.0);
    if (vertex_index == 1u) {
        position = vec2<f32>(3.0, -1.0);
    } else if (vertex_index == 2u) {
        position = vec2<f32>(-1.0, 3.0);
    }

    var out: VertexOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = vec2<f32>((position.x + 1.0) * 0.5, 1.0 - (position.y + 1.0) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(ui_texture, ui_sampler, in.uv);
}
