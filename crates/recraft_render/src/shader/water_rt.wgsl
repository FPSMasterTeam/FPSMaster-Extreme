// Hardware ray-traced reflection for the water shader (prepended to water.wgsl). Casts
// the reflected ray at the TLAS instead of marching the depth buffer, so reflections
// aren't bounded by the screen — off-screen terrain and anything behind the camera
// reflect correctly. `lighting` is declared in water.wgsl (use-before-declaration).
enable wgpu_ray_query;

// Group 3 = the RayTracer's shared bind group (TLAS + RtParams + per-triangle colours).
@group(3) @binding(0) var refl_tlas: acceleration_structure;
struct ReflRtParams {
    config: vec4<f32>,
    quality: vec4<f32>,
};
@group(3) @binding(1) var<uniform> refl_rt: ReflRtParams;
@group(3) @binding(2) var<storage, read> refl_tri_colors: array<u32>;

// Reflected colour in rgb, hit confidence in a (0 = miss → caller falls back to sky).
fn reflect_ray(origin: vec3<f32>, dir: vec3<f32>) -> vec4<f32> {
    var rq: ray_query;
    // Closest hit (no terminate flag); range matches the TLAS coverage.
    rayQueryInitialize(&rq, refl_tlas, RayDesc(0x00u, 0xFFu, 0.05, 160.0, origin, dir));
    rayQueryProceed(&rq);
    let it = rayQueryGetCommittedIntersection(&rq);
    if (it.kind == 0u) {
        return vec4<f32>(0.0); // miss → caller reflects the sky
    }
    // The reflected surface's real colour (atlas texel x tint) from the shared pool.
    let packed = refl_tri_colors[it.instance_custom_data + it.primitive_index];
    let albedo = vec3<f32>(
        f32((packed >> 16u) & 0xFFu),
        f32((packed >> 8u) & 0xFFu),
        f32(packed & 0xFFu),
    ) * (1.0 / 255.0);
    // Cheap relight of the reflected surface: ambient + sunlight if the hit point is
    // not in shadow (a second ray toward the sun).
    let hit = origin + dir * it.t;
    let sun = lighting.sun_dir.xyz;
    var lit = lighting.ambient.rgb * 0.6;
    var srq: ray_query;
    rayQueryInitialize(&srq, refl_tlas, RayDesc(0x04u, 0xFFu, 0.05, 160.0, hit + sun * 0.05, sun));
    rayQueryProceed(&srq);
    if (rayQueryGetCommittedIntersection(&srq).kind == 0u) {
        lit = lit + lighting.sun_color.rgb * 0.9;
    }
    return vec4<f32>(albedo * lit, 1.0);
}
