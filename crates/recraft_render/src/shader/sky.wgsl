// Fullscreen sky gradient. A view ray is reconstructed per pixel from the
// inverse rotation-only view-projection, so the horizon sits where the camera
// actually looks. The gradient runs from the horizon/fog color up to the zenith
// sky color, with an orange sunrise/sunset glow added near the horizon toward
// the sun.

struct Sky {
    inv_view_proj: mat4x4<f32>,
    horizon: vec4<f32>,
    zenith: vec4<f32>,
    // xyz = world-space sun direction, w = sunset glow strength.
    sun_dir: vec4<f32>,
    sunset: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> sky: Sky;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let pos = positions[vertex_index];
    var out: VertexOutput;
    out.clip_position = vec4<f32>(pos, 1.0, 1.0);
    out.ndc = pos;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Reconstruct the world-space view direction for this pixel.
    let world = sky.inv_view_proj * vec4<f32>(input.ndc, 1.0, 1.0);
    let dir = normalize(world.xyz / world.w);

    // Horizon → zenith gradient, weighted toward the horizon (most of the dome
    // is the deep sky color, blending to fog only near eye level).
    let t = smoothstep(0.0, 0.55, clamp(dir.y, 0.0, 1.0));
    var color = mix(sky.horizon.rgb, sky.zenith.rgb, t);

    // Sunrise/sunset glow: a horizon band facing the sun, only at dawn/dusk.
    let glow = sky.sun_dir.w;
    if (glow > 0.0) {
        let toward = max(dot(dir, sky.sun_dir.xyz), 0.0);
        let band = 1.0 - smoothstep(0.0, 0.35, abs(dir.y));
        let amount = clamp(pow(toward, 4.0) * band * glow, 0.0, 1.0);
        color = mix(color, sky.sunset.rgb, amount);
    }

    return vec4<f32>(color, 1.0);
}
