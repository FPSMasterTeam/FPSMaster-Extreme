// Hardware ray-tracing helpers prepended to chunk.wgsl when ray tracing is active
// (the device exposes EXPERIMENTAL_RAY_QUERY and the user enabled it). Provides the
// `sun_visibility` and `rt_ao_factor` the chunk fragment shader calls — here they
// cast rays against the world TLAS for sharp/soft sun shadows and ambient occlusion.
//
// The matching no-op version is rt_stub.wgsl (default build, no ray_query). Both
// reference `lighting` / `sun_shadow` declared later in chunk.wgsl (WGSL allows
// module-scope use-before-declaration).

// Ray queries are an experimental WGSL extension; this directive must precede every
// other module item (it does — rt_common is prepended ahead of chunk.wgsl).
enable wgpu_ray_query;

// World acceleration structure: per-section opaque geometry, instanced into a
// camera-relative TLAS (same frame as `in.world_pos`), rebuilt each frame.
@group(3) @binding(0) var rt_tlas: acceleration_structure;

struct RtParams {
    // x: shadow ray max length (blocks), y: AO ray max length (blocks),
    // z: frame counter (rotates the sample pattern so TAA averages the noise),
    // w: sun angular radius in radians (soft-shadow penumbra width).
    config: vec4<f32>,
    // x: shadow sample count, y: AO sample count, z: AO strength (0..1), w: unused.
    quality: vec4<f32>,
};
@group(3) @binding(1) var<uniform> rt: RtParams;

// Ray flag: stop at the first hit. All BLAS geometry is OPAQUE, so any committed
// hit means the point is occluded — no need to find the closest one.
const RT_TERMINATE_ON_FIRST_HIT: u32 = 0x04u;

// PCG hash (u32 -> u32): high-quality avalanche, the basis for a per-pixel stateful
// RNG. Seeding from the SCREEN pixel coordinate (not world position) gives the
// high-frequency white noise a temporal resolve (TAA) can average — a position-based
// hash produces low-frequency, structured ("moiré") patterns on flat surfaces.
fn rt_pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// Stateful RNG: advance the seed and return a uniform float in [0, 1).
fn rt_rand(seed: ptr<function, u32>) -> f32 {
    *seed = rt_pcg(*seed);
    return f32(*seed) * (1.0 / 4294967296.0);
}

// Decorrelated per-pixel, per-frame seed. `salt` separates the shadow vs AO streams.
fn rt_seed(pixel: vec2<f32>, salt: u32) -> u32 {
    let frame = u32(rt.config.z);
    return rt_pcg(
        u32(pixel.x) * 3266489917u
        + u32(pixel.y) * 668265263u
        + frame * 374761393u
        + salt,
    );
}

// Orthonormal basis around a unit normal (Duff et al. branchless method).
fn rt_basis(n: vec3<f32>) -> mat3x3<f32> {
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    let t = vec3<f32>(1.0 + s * n.x * n.x * a, s * b, -s * n.x);
    let bt = vec3<f32>(b, s + n.y * n.y * a, -n.y);
    return mat3x3<f32>(t, bt, n);
}

// True if a ray from `origin` along `dir` hits any opaque geometry within `tmax`.
fn rt_occluded(origin: vec3<f32>, dir: vec3<f32>, tmax: f32) -> bool {
    var rq: ray_query;
    rayQueryInitialize(&rq, rt_tlas,
        RayDesc(RT_TERMINATE_ON_FIRST_HIT, 0xFFu, 0.02, tmax, origin, dir));
    // Opaque-only geometry: traversal commits hits itself, so just drain it.
    loop {
        if (!rayQueryProceed(&rq)) { break; }
    }
    let hit = rayQueryGetCommittedIntersection(&rq);
    return hit.kind != 0u; // 0 == RAY_QUERY_INTERSECTION_NONE (a miss)
}

// Fraction of the sun visible at this surface point (1 = lit, 0 = shadowed). A
// single ray gives hard shadows; sampling the sun's angular disk gives soft
// penumbrae (resolved by TAA). `geo_n` is the geometric face normal; `pixel` is the
// fragment's framebuffer coordinate (the RNG seed).
fn sun_visibility(world_pos: vec3<f32>, geo_n: vec3<f32>, ndotl: f32, pixel: vec2<f32>) -> f32 {
    // Surfaces facing away from the sun are self-shadowed; skip the trace.
    if (ndotl <= 0.0) { return 0.0; }
    let origin = world_pos + geo_n * 0.03;
    let sun = normalize(lighting.sun_dir.xyz);
    let tmax = rt.config.x;
    let samples = i32(rt.quality.x);
    let radius = rt.config.w;
    if (samples <= 1 || radius <= 0.0) {
        // Hard shadow: one ray, no noise.
        return select(1.0, 0.0, rt_occluded(origin, sun, tmax));
    }
    let basis = rt_basis(sun);
    var seed = rt_seed(pixel, 0u);
    var lit = 0.0;
    for (var i = 0; i < samples; i = i + 1) {
        let u1 = rt_rand(&seed);
        let u2 = rt_rand(&seed);
        // Uniform sample of the sun's angular disk, tilted around the sun dir.
        let r = radius * sqrt(u1);
        let a = 6.2831853 * u2;
        let dir = normalize(sun + basis * vec3<f32>(cos(a) * r, sin(a) * r, 0.0));
        lit = lit + select(1.0, 0.0, rt_occluded(origin, dir, tmax));
    }
    return lit / f32(samples);
}

// Ray-traced ambient occlusion: cosine-weighted hemisphere rays around the
// geometric normal; the fraction that hit nearby geometry darkens ambient light.
fn rt_ao_factor(world_pos: vec3<f32>, geo_n: vec3<f32>, pixel: vec2<f32>) -> f32 {
    let samples = i32(rt.quality.y);
    if (samples <= 0) { return 1.0; }
    let origin = world_pos + geo_n * 0.03;
    let basis = rt_basis(geo_n);
    let tmax = rt.config.y;
    var seed = rt_seed(pixel, 0x9e3779b9u);
    var occ = 0.0;
    for (var i = 0; i < samples; i = i + 1) {
        let u1 = rt_rand(&seed);
        let u2 = rt_rand(&seed);
        let r = sqrt(u1);
        let a = 6.2831853 * u2;
        let local = vec3<f32>(r * cos(a), r * sin(a), sqrt(max(0.0, 1.0 - u1)));
        let dir = basis * local;
        if (rt_occluded(origin, dir, tmax)) { occ = occ + 1.0; }
    }
    let ao = 1.0 - (occ / f32(samples)) * rt.quality.z;
    return clamp(ao, 0.0, 1.0);
}

// Direct-sun sky gating. With RT the ray-traced shadow is the authority on sun
// visibility, so within the TLAS range we DROP the voxel sky-light gating (return 1) —
// the sun reaches wherever a ray to it is unobstructed, regardless of how the voxel
// skylight propagated. Beyond the TLAS range (nothing to shadow against) fall back to
// the voxel sky-light so distant overhangs stay shaded, blended over 120..160 blocks
// so there's no hard line at the range boundary. `world_pos` is camera-relative, so
// its length is the view distance.
fn sun_sky_gate(sky: f32, world_pos: vec3<f32>) -> f32 {
    let beyond = smoothstep(120.0, 160.0, length(world_pos));
    return mix(1.0, sky, beyond);
}

// Flat sky-ambient scale. When the screen-space sky GI is active (AO samples > 0) it
// supplies the directional sky lighting additively, so dial the flat voxel sky-ambient
// down to avoid double-counting (a floor is kept so unlit cavities aren't pitch black).
// Low quality (no GI pass) keeps the full flat ambient.
fn gi_ambient_scale() -> f32 {
    return select(1.0, 0.4, rt.quality.y > 0.0);
}
