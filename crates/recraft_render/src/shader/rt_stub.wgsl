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

// Direct-sun sky gating. No RT here: the voxel sky-light gates the sun (the sun only
// reaches where skylight propagated), as it always has.
fn sun_sky_gate(sky: f32, world_pos: vec3<f32>) -> f32 {
    return sky;
}

// Flat sky-ambient scale. No RT sky GI here, so keep the full voxel sky-ambient.
fn gi_ambient_scale() -> f32 {
    return 1.0;
}

// No ray-traced reflection in the default build: a == 0 so the chunk shader adds
// nothing (it keeps its crude ambient env-spec).
fn rt_reflect(origin: vec3<f32>, dir: vec3<f32>) -> vec4<f32> {
    return vec4<f32>(0.0);
}

// No RT point lights: keep the full voxel block-light, add nothing.
fn torch_voxel_scale() -> f32 {
    return 1.0;
}
fn block_lights(world_pos: vec3<f32>, n: vec3<f32>, pixel: vec2<f32>) -> vec3<f32> {
    return vec3<f32>(0.0);
}
fn block_lights_raw(world_pos: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(0.0);
}
