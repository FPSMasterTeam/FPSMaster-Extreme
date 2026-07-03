// Screen-space reflection for the water shader (prepended to water.wgsl). Marches the
// reflected ray through the copied opaque-scene depth buffer. `camera` is declared in
// water.wgsl (WGSL allows module-scope use-before-declaration).

// Group 3: copied opaque scene + depth + camera.
struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};
@group(3) @binding(0) var scene_tex: texture_2d<f32>;
@group(3) @binding(1) var scene_sampler: sampler;
// World depth as an unfilterable float texture (texelFetch), not texture_depth_2d.
@group(3) @binding(2) var depth_tex: texture_2d<f32>;
@group(3) @binding(3) var<uniform> ssr_cam: PostCamera;

// Reflected colour in rgb, hit confidence in a (0 = miss → caller falls back to sky).
fn reflect_ray(origin: vec3<f32>, dir: vec3<f32>) -> vec4<f32> {
    let dims = vec2<f32>(textureDimensions(depth_tex));
    var p = origin;
    var step = 0.5;
    for (var i = 0; i < 18; i = i + 1) {
        p = p + dir * step;
        step = step * 1.22;
        let clip = camera.view_proj * vec4<f32>(p, 1.0);
        if (clip.w <= 0.0) {
            break;
        }
        let ndc = clip.xyz / clip.w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
            break;
        }
        let px = vec2<i32>(clamp(uv * dims, vec2<f32>(0.0), dims - 1.0));
        let scene_depth = textureLoad(depth_tex, px, 0).r;
        let delta = ndc.z - scene_depth;
        if (delta > 0.00002 && delta < 0.0025) {
            let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
            return vec4<f32>(
                textureSampleLevel(scene_tex, scene_sampler, uv, 0.0).rgb,
                smoothstep(0.0, 0.12, edge),
            );
        }
    }
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}

// Screen-space refraction with depth-based absorption. Samples the copied (textured)
// scene at the fragment's screen UV, shifted by the wave normal so it ripples; then
// attenuates it by the water-column depth (reconstructed from the bottom depth at the
// refracted UV) — Beer-Lambert, red absorbed fastest — so shallow water is clear and
// deep water darkens to a teal body. a == 1 (the shader provides the see-through).
fn refract_water(clip_xy: vec2<f32>, surf_pos: vec3<f32>, n: vec3<f32>) -> vec4<f32> {
    let dims = vec2<f32>(textureDimensions(scene_tex));
    let uv = clip_xy / dims;
    let suv = clamp(uv + n.xz * 0.06, vec2<f32>(0.0), vec2<f32>(1.0));
    let scene = textureSampleLevel(scene_tex, scene_sampler, suv, 0.0).rgb;
    // Reconstruct the bottom world position from the depth at the refracted UV.
    let px = vec2<i32>(clamp(suv * dims, vec2<f32>(0.0), dims - vec2<f32>(1.0)));
    let bd = textureLoad(depth_tex, px, 0).r;
    let clip = vec4<f32>(suv.x * 2.0 - 1.0, 1.0 - suv.y * 2.0, bd, 1.0);
    let bw = ssr_cam.inv_view_proj * clip;
    let bottom = bw.xyz / bw.w;
    let depth = clamp(length(bottom - surf_pos), 0.0, 32.0);
    // Beer-Lambert transmittance per channel (red absorbed fastest → deep = blue/teal).
    let trans = exp(-depth * vec3<f32>(0.42, 0.16, 0.10));
    let scatter = vec3<f32>(0.015, 0.07, 0.11); // deep-water in-scatter colour
    let underwater = scene * trans + scatter * (1.0 - trans);
    return vec4<f32>(underwater, 1.0);
}
