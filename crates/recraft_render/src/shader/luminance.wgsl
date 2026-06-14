// Average-luminance reduction for auto-exposure: one fullscreen draw into a 1x1
// R32Float target that samples a coarse grid of the HDR scene and outputs the
// log-average luminance (geometric mean), which the post pass turns into an
// exposure multiplier.

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
@group(0) @binding(2) var prev_tex: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    let luma = vec3<f32>(0.2126, 0.7152, 0.0722);
    var sum = 0.0;
    let n = 8;
    for (var i = 0; i < n; i = i + 1) {
        for (var j = 0; j < n; j = j + 1) {
            let uv = (vec2<f32>(f32(i), f32(j)) + 0.5) / f32(n);
            let c = textureSampleLevel(scene_tex, scene_sampler, uv, 0.0).rgb;
            sum = sum + log(max(dot(c, luma), 1e-4));
        }
    }
    let avg = exp(sum / f32(n * n));
    // Eye adaptation: ease the stored luminance toward this frame's average so
    // exposure changes smoothly instead of flickering. Seed from `avg` when the
    // history is uninitialised.
    let prev = textureLoad(prev_tex, vec2<i32>(0, 0), 0).r;
    let base = select(avg, prev, prev > 0.0001);
    let adapted = mix(base, avg, 0.03);
    return vec4<f32>(adapted, 0.0, 0.0, 1.0);
}
