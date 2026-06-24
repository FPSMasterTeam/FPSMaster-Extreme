// No-op ray-tracing stubs prepended to chunk.wgsl on the DEFAULT build (no
// EXPERIMENTAL_RAY_QUERY feature). Keeps the chunk shader's call sites identical to
// the ray-traced variant (rt_common.wgsl) while compiling everywhere: there is no
// `acceleration_structure` binding and no `ray_query` here. `sun_visibility` falls
// back to the rasterized shadow map; `rt_ao_factor` is a pass-through.
//
// References `lighting` / `sun_shadow` declared later in chunk.wgsl (WGSL allows
// module-scope use-before-declaration).

fn sun_visibility(world_pos: vec3<f32>, geo_n: vec3<f32>, ndotl: f32, pixel: vec2<f32>) -> f32 {
    if (lighting.flags.y > 0.5) {
        return sun_shadow(world_pos, ndotl);
    }
    return 1.0;
}

fn rt_ao_factor(world_pos: vec3<f32>, geo_n: vec3<f32>, pixel: vec2<f32>) -> f32 {
    return 1.0;
}
