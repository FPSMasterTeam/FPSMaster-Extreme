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
// Per-triangle packed surface colour (8:8:8 RGB), indexed by a hit's
// instance_custom_data (section base) + primitive_index — for ray-traced reflections.
@group(3) @binding(2) var<storage, read> rt_tri_colors: array<u32>;

// Emissive point lights (the nearest in-range emitters, camera-relative). The chunk
// fragment loops over these for ray-traced point lighting with hard shadows.
struct RtLight {
    pos_radius: vec4<f32>, // xyz: camera-relative position, w: radius (blocks)
    color: vec4<f32>,      // rgb: colour, w: intensity (0 = empty slot)
};
@group(3) @binding(3) var<storage, read> rt_lights: array<RtLight>;
// Per-triangle atlas UVs (3 packed Unorm16x2 per triangle) + section-local vertex
// positions (9 f32 per triangle). Reflections use them to reconstruct the hit UV and
// sample the REAL texture (the flat per-triangle colour has no texture detail) and to
// alpha-test cutout holes (grass/flowers/leaves) instead of reflecting them as solid.
@group(3) @binding(4) var<storage, read> rt_tri_normals: array<u32>;
@group(3) @binding(5) var<storage, read> rt_tri_uvs: array<u32>;
@group(3) @binding(6) var<storage, read> rt_tri_positions: array<f32>;

fn rt_unpack_uv(p: u32) -> vec2<f32> {
    return vec2<f32>(f32(p & 0xFFFFu), f32((p >> 16u) & 0xFFFFu)) * (1.0 / 65535.0);
}
fn rt_unpack_normal(p: u32) -> vec3<f32> {
    let b = vec3<f32>(f32((p >> 16u) & 0xFFu), f32((p >> 8u) & 0xFFu), f32(p & 0xFFu));
    return normalize((b - 128.0) / 127.0);
}

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
        RayDesc(RT_TERMINATE_ON_FIRST_HIT, 0x01u, 0.02, tmax, origin, dir));
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

// Cheap sky radiance for a reflected ray that escapes to the sky (horizon = fog colour
// up to a brighter zenith, plus a soft sun disc).
fn rt_sky(dir: vec3<f32>) -> vec3<f32> {
    let up = clamp(dir.y, 0.0, 1.0);
    let horizon = lighting.fog_color.rgb;
    let zenith = horizon * 0.6 + vec3<f32>(0.10, 0.20, 0.40) * camera.sky_brightness;
    var col = mix(horizon, zenith, up);
    let sun = max(dot(normalize(dir), lighting.sun_dir.xyz), 0.0);
    col = col + lighting.sun_color.rgb * pow(sun, 64.0) * 1.5;
    return col;
}

// Hardware ray-traced reflection: cast `dir` from `origin` at the TLAS. Miss -> the sky;
// hit -> the reflected surface's real colour (per-triangle pool) relit by ambient + a
// sun shadow ray. Returns the reflected radiance; a==1 (the caller weights it by the
// surface's reflectivity). The no-op stub returns a==0.
fn rt_reflect(origin: vec3<f32>, dir: vec3<f32>) -> vec4<f32> {
    var ro = origin;
    // A few iterations so the reflection ray passes THROUGH cutout holes (grass/flowers/
    // leaves) instead of stopping on their transparent texels and reflecting a solid quad.
    for (var iter = 0; iter < 4; iter = iter + 1) {
        var rq: ray_query;
        rayQueryInitialize(&rq, rt_tlas, RayDesc(0u, 0x01u, 0.02, 160.0, ro, dir));
        loop { if (!rayQueryProceed(&rq)) { break; } }
        let it = rayQueryGetCommittedIntersection(&rq);
        if (it.kind == 0u) {
            return vec4<f32>(rt_sky(dir), 1.0);
        }
        let idx = it.instance_custom_data + it.primitive_index;
        let packed = rt_tri_colors[idx];
        let coverage = f32((packed >> 27u) & 0x1Fu) / 31.0;
        let hit = ro + dir * it.t;
        // Reconstruct the hit UV from the triangle's section-local positions (the RT hit
        // gives no UV) — same barycentric trick as the path tracer.
        let pbase = idx * 9u;
        let p0 = vec3<f32>(rt_tri_positions[pbase], rt_tri_positions[pbase + 1u], rt_tri_positions[pbase + 2u]);
        let p1 = vec3<f32>(rt_tri_positions[pbase + 3u], rt_tri_positions[pbase + 4u], rt_tri_positions[pbase + 5u]);
        let p2 = vec3<f32>(rt_tri_positions[pbase + 6u], rt_tri_positions[pbase + 7u], rt_tri_positions[pbase + 8u]);
        let hp = hit - it.object_to_world[3];
        let e1 = p1 - p0;
        let e2 = p2 - p0;
        let ph = hp - p0;
        let d00 = dot(e1, e1);
        let d01 = dot(e1, e2);
        let d11 = dot(e2, e2);
        let d20 = dot(ph, e1);
        let d21 = dot(ph, e2);
        let denom = max(d00 * d11 - d01 * d01, 1e-8);
        let b1 = (d11 * d20 - d01 * d21) / denom;
        let b2 = (d00 * d21 - d01 * d20) / denom;
        let b0 = 1.0 - b1 - b2;
        let uvbase = idx * 3u;
        let t0 = rt_unpack_uv(rt_tri_uvs[uvbase]);
        let t1 = rt_unpack_uv(rt_tri_uvs[uvbase + 1u]);
        let t2 = rt_unpack_uv(rt_tri_uvs[uvbase + 2u]);
        var uv = t0 * b0 + t1 * b1 + t2 * b2;
        let tmn = min(t0, min(t1, t2));
        let tmx = max(t0, max(t1, t2));
        let inset = (tmx - tmn) * 0.001;
        uv = clamp(uv, tmn + inset, tmx - inset);
        let texel = textureSampleLevel(block_atlas, block_sampler, uv, 0.0);
        // Cutout hole → see through it.
        if (coverage < 0.99 && texel.a < 0.5) {
            ro = hit + dir * 0.002;
            continue;
        }
        // Biome tint for greyscale texels (grass/leaves), like the main shader.
        let avg = vec3<f32>(
            f32((packed >> 16u) & 0xFFu),
            f32((packed >> 8u) & 0xFFu),
            f32(packed & 0xFFu),
        ) * (1.0 / 255.0);
        let tint = avg / max(max(avg.r, max(avg.g, avg.b)), 0.001);
        let sat = max(texel.r, max(texel.g, texel.b)) - min(texel.r, min(texel.g, texel.b));
        let greyness = clamp(1.0 - sat * 3.0, 0.0, 1.0);
        let albedo = texel.rgb * mix(vec3<f32>(1.0), tint, greyness);
        // Relight with the hit's OWN normal: ambient + sun × N·L × shadow. Without the N·L
        // term every reflected face came out at the same brightness — a flat, unlit-looking
        // mirror; this restores the scene's directional shading inside the reflection.
        let hn = rt_unpack_normal(rt_tri_normals[idx]);
        let sun = lighting.sun_dir.xyz;
        let ndl = max(dot(hn, sun), 0.0);
        var lit = lighting.ambient.rgb * 0.6;
        if (ndl > 0.0 && !rt_occluded(hit + hn * 0.03, sun, 160.0)) {
            lit = lit + lighting.sun_color.rgb * ndl;
        }
        return vec4<f32>(albedo * lit, 1.0);
    }
    return vec4<f32>(rt_sky(dir), 1.0);
}

// Voxel block-light scale: with RT point lights active, dial the smooth voxel torch
// light down (the RT lights supply the sharp, shadowed, coloured point lighting) but
// keep a base so emitters beyond the light buffer still tint nearby surfaces.
fn torch_voxel_scale() -> f32 {
    return 0.45;
}

// Ray-traced point lighting from emissive blocks: for each in-range light, distance
// attenuation × N·L × a hard shadow ray (occluded → no contribution). The 128-slot loop
// early-skips empty / out-of-range / back-facing lights, so only a few nearby emitters
// actually cast a ray. Returns the coloured light to add to the block-light term.
fn block_lights(world_pos: vec3<f32>, n: vec3<f32>, pixel: vec2<f32>) -> vec3<f32> {
    var accum = vec3<f32>(0.0);
    let count = arrayLength(&rt_lights);
    let origin = world_pos + n * 0.04;
    var seed = rt_seed(pixel, 0x2c1b3e9du);
    for (var i = 0u; i < count; i = i + 1u) {
        let L = rt_lights[i];
        if (L.color.w <= 0.0) {
            continue;
        }
        // Sample a random point in the emitter's 1-block CUBE (area light) rather than its
        // centre: a cube light casts a soft, spread pool with penumbra shadows instead of a
        // point's tight circular hot-spot. One sample/pixel/frame; TAA averages it smooth.
        let jitter = vec3<f32>(rt_rand(&seed), rt_rand(&seed), rt_rand(&seed)) - 0.5;
        let to = (L.pos_radius.xyz + jitter) - world_pos;
        let dist = length(to);
        if (dist >= L.pos_radius.w) {
            continue;
        }
        let dir = to / max(dist, 1e-4);
        let ndotl = max(dot(n, dir), 0.0);
        if (ndotl <= 0.0) {
            continue;
        }
        let r = dist / L.pos_radius.w;
        // Inverse-square falloff with a smooth radius window: a bright core fading off
        // gradually (a natural halo) rather than a flat, hard-edged disc.
        let window = clamp(1.0 - r * r * r * r, 0.0, 1.0);
        let atten = window / (1.0 + dist * dist * 0.2);
        // Solid occlusion (mask 0x01): a wall between the surface and the light blocks it.
        var rq: ray_query;
        rayQueryInitialize(&rq, rt_tlas, RayDesc(RT_TERMINATE_ON_FIRST_HIT, 0x01u, 0.04, dist - 0.7, origin, dir));
        rayQueryProceed(&rq);
        if (rayQueryGetCommittedIntersection(&rq).kind != 0u) {
            continue;
        }
        // Stained glass in the path (mask 0x02) tints the light by the glass colour —
        // coloured light through coloured glass. The closest pane's pool colour multiplies
        // the contribution (one glass layer; good enough for a glass box round a light).
        var light_color = L.color.rgb;
        var gq: ray_query;
        rayQueryInitialize(&gq, rt_tlas, RayDesc(0u, 0x02u, 0.04, dist - 0.7, origin, dir));
        // Closest-hit traversal: drain the query so it commits the nearest glass pane
        // (a single proceed may not finish the BVH walk → no hit → light not tinted).
        loop {
            if (!rayQueryProceed(&gq)) { break; }
        }
        let git = rayQueryGetCommittedIntersection(&gq);
        if (git.kind != 0u) {
            let packed = rt_tri_colors[git.instance_custom_data + git.primitive_index];
            let gc = vec3<f32>(
                f32((packed >> 16u) & 0xFFu),
                f32((packed >> 8u) & 0xFFu),
                f32(packed & 0xFFu),
            ) * (1.0 / 255.0);
            // The glass FILTERS the light to its own colour. Replace the source hue (not
            // multiply) and saturate to the brightest channel, so red glass → clean red
            // light and blue → clean blue — distinct per colour, not the muddied source.
            let mc = max(gc.r, max(gc.g, gc.b));
            light_color = gc / max(mc, 0.04);
        }
        accum = accum + light_color * (L.color.w * atten * ndotl);
    }
    return accum;
}

// Two-sided raw point light for lighting the stained-glass SURFACE itself (so a light
// behind the pane makes the whole pane glow): abs(N·L) lights both faces, solid-only
// occlusion (the glass pane, mask 0x02, doesn't occlude), no glass tint (the caller
// multiplies by the glass colour). Returns the light reaching the surface.
fn block_lights_raw(world_pos: vec3<f32>, n: vec3<f32>, pixel: vec2<f32>) -> vec3<f32> {
    var accum = vec3<f32>(0.0);
    let count = arrayLength(&rt_lights);
    var seed = rt_seed(pixel, 0x6b1e57a3u);
    for (var i = 0u; i < count; i = i + 1u) {
        let L = rt_lights[i];
        if (L.color.w <= 0.0) {
            continue;
        }
        // Same cube area-light sampling as block_lights, so a backlit glass pane is lit by
        // the emitter's whole cube (soft) — not a point that paints a circular hot-spot.
        let jitter = vec3<f32>(rt_rand(&seed), rt_rand(&seed), rt_rand(&seed)) - 0.5;
        let to = (L.pos_radius.xyz + jitter) - world_pos;
        let dist = length(to);
        if (dist >= L.pos_radius.w) {
            continue;
        }
        let dir = to / max(dist, 1e-4);
        let nl = abs(dot(n, dir));
        let r = dist / L.pos_radius.w;
        // Inverse-square falloff with a smooth radius window: a bright core fading off
        // gradually (a natural halo) rather than a flat, hard-edged disc.
        let window = clamp(1.0 - r * r * r * r, 0.0, 1.0);
        let atten = window / (1.0 + dist * dist * 0.2);
        var rq: ray_query;
        rayQueryInitialize(&rq, rt_tlas, RayDesc(RT_TERMINATE_ON_FIRST_HIT, 0x01u, 0.04, dist - 0.7, world_pos + dir * 0.05, dir));
        rayQueryProceed(&rq);
        if (rayQueryGetCommittedIntersection(&rq).kind != 0u) {
            continue;
        }
        accum = accum + L.color.rgb * (L.color.w * atten * nl);
    }
    return accum;
}

// Flat sky-ambient scale. When the screen-space sky GI is active (AO samples > 0) it
// supplies the directional sky lighting additively, so dial the flat voxel sky-ambient
// down to avoid double-counting (a floor is kept so unlit cavities aren't pitch black).
// Low quality (no GI pass) keeps the full flat ambient.
fn gi_ambient_scale() -> f32 {
    // Low flat floor so the occlusion-aware RT sky GI dominates the ambient — that's
    // what makes corners/cavities visibly darker than open sky (the AO read).
    return select(1.0, 0.18, rt.quality.y > 0.0);
}
