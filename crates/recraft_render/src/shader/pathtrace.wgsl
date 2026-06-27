// Full path tracer (experimental "Path Traced" mode). Traces primary rays from the
// camera into the TLAS, then bounces diffusely with next-event estimation to the sun
// and the emissive-block point lights, accumulating radiance. Miss = sky. The result is
// stochastic; it is blended into a temporal accumulation buffer (reset on camera move)
// by the host, then tonemapped by the normal post chain.
enable wgpu_ray_query;

// group(0): the RayTracer shared bind group (same layout the chunk RT pipelines use).
@group(0) @binding(0) var tlas: acceleration_structure;
struct RtParams {
    config: vec4<f32>, // z = frame counter (RNG seed)
    quality: vec4<f32>,
};
@group(0) @binding(1) var<uniform> rt: RtParams;
@group(0) @binding(2) var<storage, read> tri_colors: array<u32>;
struct PtLight {
    pos_radius: vec4<f32>,
    color: vec4<f32>, // rgb colour, w intensity (0 = empty)
};
@group(0) @binding(3) var<storage, read> pt_lights: array<PtLight>;
@group(0) @binding(4) var<storage, read> tri_normals: array<u32>;
// 3 packed atlas UVs per triangle (Unorm16 x2 per u32), indexed tri_index * 3.
@group(0) @binding(5) var<storage, read> tri_uvs: array<u32>;
// 3 section-local vertex positions per triangle (9 f32), indexed tri_index * 9.
@group(0) @binding(6) var<storage, read> tri_positions: array<f32>;

// group(1): camera (primary-ray reconstruction) + sky.
struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};
@group(1) @binding(0) var<uniform> cam: PostCamera;
struct Sky {
    inv_view_proj: mat4x4<f32>,
    horizon: vec4<f32>,
    zenith: vec4<f32>,
    sun_dir: vec4<f32>, // xyz dir to sun, w sunset glow
    sunset: vec4<f32>,
    camera_pos: vec4<f32>,
    cloud_params: vec4<f32>, // w = day factor
};
@group(1) @binding(1) var<uniform> sky: Sky;
@group(1) @binding(2) var atlas: texture_2d<f32>;
@group(1) @binding(3) var atlas_samp: sampler;

fn unpack_uv(p: u32) -> vec2<f32> {
    return vec2<f32>(f32(p & 0xFFFFu), f32((p >> 16u) & 0xFFFFu)) * (1.0 / 65535.0);
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    out.uv = vec2<f32>(x, y);
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

fn sky_color(dir: vec3<f32>) -> vec3<f32> {
    let t = smoothstep(0.0, 0.55, clamp(dir.y, 0.0, 1.0));
    var color = mix(sky.horizon.rgb, sky.zenith.rgb, t);
    let glow = sky.sun_dir.w;
    if (glow > 0.0) {
        let toward = max(dot(dir, sky.sun_dir.xyz), 0.0);
        let band = 1.0 - smoothstep(0.0, 0.35, abs(dir.y));
        let amount = clamp(pow(toward, 4.0) * band * glow, 0.0, 1.0);
        color = mix(color, sky.sunset.rgb, amount);
    }
    // A bright sun disc so it can light the scene + show in reflections.
    let to_sun = max(dot(dir, sky.sun_dir.xyz), 0.0);
    color += vec3<f32>(1.0, 0.96, 0.86) * pow(to_sun, 350.0) * 8.0 * max(sky.cloud_params.w, 0.0);
    return color;
}

// PCG RNG: returns a float in [0,1) and advances the state.
fn rand(state: ptr<function, u32>) -> f32 {
    *state = *state * 747796405u + 2891336453u;
    var word = ((*state >> ((*state >> 28u) + 4u)) ^ *state) * 277803737u;
    word = (word >> 22u) ^ word;
    return f32(word) / 4294967296.0;
}

// Cosine-weighted hemisphere sample around n (BRDF importance sampling for diffuse).
fn cosine_dir(n: vec3<f32>, seed: ptr<function, u32>) -> vec3<f32> {
    let r1 = rand(seed);
    let r2 = rand(seed);
    let phi = 6.2831853 * r1;
    let st = sqrt(r2);
    let x = cos(phi) * st;
    let y = sin(phi) * st;
    let z = sqrt(1.0 - r2);
    var up = vec3<f32>(0.0, 1.0, 0.0);
    if (abs(n.y) > 0.99) { up = vec3<f32>(1.0, 0.0, 0.0); }
    let tx = normalize(cross(up, n));
    let ty = cross(n, tx);
    return normalize(tx * x + ty * y + n * z);
}

fn unpack_rgb(p: u32) -> vec3<f32> {
    return vec3<f32>(f32((p >> 16u) & 0xFFu), f32((p >> 8u) & 0xFFu), f32(p & 0xFFu)) * (1.0 / 255.0);
}

fn unpack_normal(p: u32) -> vec3<f32> {
    let b = vec3<f32>(f32((p >> 16u) & 0xFFu), f32((p >> 8u) & 0xFFu), f32(p & 0xFFu));
    return normalize((b - 128.0) / 127.0);
}

fn occluded(origin: vec3<f32>, dir: vec3<f32>, tmax: f32) -> bool {
    var rq: ray_query;
    rayQueryInitialize(&rq, tlas, RayDesc(0x04u, 0x01u, 0.02, tmax, origin, dir));
    rayQueryProceed(&rq);
    return rayQueryGetCommittedIntersection(&rq).kind != 0u;
}

const MAX_BOUNCES: i32 = 3;

fn trace_path(ro: vec3<f32>, rd: vec3<f32>, seed: ptr<function, u32>) -> vec3<f32> {
    var origin = ro;
    var dir = rd;
    var throughput = vec3<f32>(1.0);
    var radiance = vec3<f32>(0.0);
    let sun = sky.sun_dir.xyz;
    let day = max(sky.cloud_params.w, 0.0);
    // Match the raster sun_color scale (~1.0, see LightingUniform.sun_color) so the
    // path-traced world isn't over-exposed through the shared post exposure/ACES
    // tonemap — the PT was ~3.2x too bright, blowing the whole scene to white.
    let sun_color = vec3<f32>(1.0, 0.95, 0.85) * day;
    var bounces = 0;

    // One loop over ray segments. Opaque hits shade + diffuse-bounce; cutout holes, glass
    // and water are handled in-line and continue the path WITHOUT consuming a bounce.
    for (var iter = 0; iter < 24; iter = iter + 1) {
        var rq: ray_query;
        // Trace solid (0x01) + glass (0x02) + water (0x04).
        rayQueryInitialize(&rq, tlas, RayDesc(0u, 0x07u, 0.001, 1000.0, origin, dir));
        loop { if (!rayQueryProceed(&rq)) { break; } }
        let it = rayQueryGetCommittedIntersection(&rq);
        if (it.kind == 0u) {
            radiance = radiance + throughput * sky_color(dir);
            break;
        }
        let idx = it.instance_custom_data + it.primitive_index;
        let packed = tri_colors[idx];
        var n = unpack_normal(tri_normals[idx]);
        if (dot(n, dir) > 0.0) { n = -n; }
        let pos = origin + dir * it.t;
        let is_glass = ((packed >> 25u) & 1u) == 1u;
        let is_water = ((packed >> 26u) & 1u) == 1u;
        let coverage = f32((packed >> 27u) & 0x1Fu) / 31.0;
        // Barycentric-interpolated hit UV (the rasterizer-equivalent: exact, handles every
        // face orientation automatically). Now that RT forces non-greedy meshing, the
        // vertex UVs are the real atlas UVs, so this maps the texture exactly.
        let uvbase = idx * 3u;
        let t0 = unpack_uv(tri_uvs[uvbase]);
        let t1 = unpack_uv(tri_uvs[uvbase + 1u]);
        let t2 = unpack_uv(tri_uvs[uvbase + 2u]);
        // Compute the hit barycentrics OURSELVES from the triangle's vertex positions (naga
        // doesn't reliably populate it.barycentrics). object_to_world[3] is the section→
        // camera translation; subtract it to bring the hit into section-local space.
        let pbase = idx * 9u;
        let p0 = vec3<f32>(tri_positions[pbase], tri_positions[pbase + 1u], tri_positions[pbase + 2u]);
        let p1 = vec3<f32>(tri_positions[pbase + 3u], tri_positions[pbase + 4u], tri_positions[pbase + 5u]);
        let p2 = vec3<f32>(tri_positions[pbase + 6u], tri_positions[pbase + 7u], tri_positions[pbase + 8u]);
        let hp = pos - it.object_to_world[3];
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
        var uv = t0 * b0 + t1 * b1 + t2 * b2;
        // Clamp to this triangle's atlas-tile rect (anti-bleed at block edges).
        let tmin = min(t0, min(t1, t2));
        let tmax = max(t0, max(t1, t2));
        let inset = (tmax - tmin) * 0.001;
        uv = clamp(uv, tmin + inset, tmax - inset);
        let texel = textureSampleLevel(atlas, atlas_samp, uv, 0.0);
        // Biome tint ONLY for greyscale texels (grass/leaves/water are greyscale, tinted
        // by biome). Coloured textures keep their own colour, so multi-coloured blocks
        // aren't dragged toward one average hue. tint = the per-triangle avg colour's hue.
        let avg = unpack_rgb(packed);
        let tint = avg / max(max(avg.r, max(avg.g, avg.b)), 0.001);
        let sat = max(texel.r, max(texel.g, texel.b)) - min(texel.r, min(texel.g, texel.b));
        let greyness = clamp(1.0 - sat * 3.0, 0.0, 1.0);
        let albedo = texel.rgb * mix(vec3<f32>(1.0), tint, greyness);

        // Alpha-test ONLY cutout (partial-coverage) triangles — a transparent texel is a
        // hole; pass straight through (not a bounce). Solid blocks (coverage 1) are never
        // holed, so an atlas seam/edge alpha can't eat half a face. Glass/water → flags.
        if (!is_glass && !is_water && coverage < 0.99 && texel.a < 0.5) {
            origin = pos + dir * 0.002;
            continue;
        }
        // Semi-transparent (mask 0x02): alpha-blend filter by the REAL texel alpha — the
        // path picks up the surface colour where the texture is opaque and passes through
        // clear where it's transparent. Handles stained/clear glass, ice, etc. uniformly;
        // no bend. Not a bounce.
        if (is_glass) {
            throughput = throughput * mix(vec3<f32>(1.0), albedo, texel.a);
            origin = pos + dir * 0.002;
            continue;
        }
        // Water: Fresnel — stochastically reflect (sky/scene) or REFRACT (bend by the
        // water IOR) and tint by the water colour so the body is visibly coloured. More
        // water crossed = more tint (Beer-Lambert-like). Neither counts as a bounce.
        if (is_water) {
            let fres = 0.02 + 0.98 * pow(1.0 - abs(dot(n, dir)), 5.0);
            if (rand(seed) < fres) {
                dir = reflect(dir, n);
                origin = pos + n * 0.02;
            } else {
                let refr = refract(dir, n, 0.75); // air->water (1 / 1.33)
                if (dot(refr, refr) < 1e-6) {
                    dir = reflect(dir, n); // total internal reflection
                    origin = pos + n * 0.02;
                } else {
                    dir = normalize(refr);
                    throughput = throughput * vec3<f32>(0.35, 0.62, 0.78);
                    origin = pos + dir * 0.01;
                }
            }
            continue;
        }

        // Opaque surface. Emission seen on the first opaque hit (indirect emitter light is
        // the point-light NEE, so don't double-count on later bounces).
        if (((packed >> 24u) & 1u) == 1u && bounces == 0) {
            radiance = radiance + throughput * albedo * 3.0;
        }
        let surf = pos + n * 0.02;
        // NEE: sun.
        let ndl = max(dot(n, sun), 0.0);
        if (ndl > 0.0 && day > 0.0 && !occluded(surf, sun, 1000.0)) {
            radiance = radiance + throughput * albedo * sun_color * ndl;
        }
        // NEE: emissive-block point lights.
        let lcount = arrayLength(&pt_lights);
        for (var i = 0u; i < lcount; i = i + 1u) {
            let L = pt_lights[i];
            if (L.color.w <= 0.0) { continue; }
            let to = L.pos_radius.xyz - pos;
            let ld = length(to);
            if (ld >= L.pos_radius.w) { continue; }
            let ldir = to / max(ld, 1e-4);
            let lndl = max(dot(n, ldir), 0.0);
            if (lndl <= 0.0) { continue; }
            let r = ld / L.pos_radius.w;
            let win = clamp(1.0 - r * r * r * r, 0.0, 1.0);
            let atten = win / (1.0 + ld * ld * 0.2);
            if (!occluded(surf, ldir, ld - 0.7)) {
                radiance = radiance + throughput * albedo * L.color.rgb * (L.color.w * atten * lndl);
            }
        }
        // Sky-ambient floor: the colour the face's hemisphere sees, so occluded/back-lit
        // faces aren't pure black (the bounce GI refines it). Without this, 3 bounces in
        // deep terrain leave shadowed sides black — the harsh "black stripe" look.
        radiance = radiance + throughput * albedo * sky_color(n) * 0.35;

        // Diffuse bounce (cosine importance sampling → throughput *= albedo).
        if (bounces >= MAX_BOUNCES) { break; }
        bounces = bounces + 1;
        throughput = throughput * albedo;
        if (max(throughput.r, max(throughput.g, throughput.b)) < 0.02) { break; }
        dir = cosine_dir(n, seed);
        origin = surf;
    }
    return radiance;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let ndc = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let near = cam.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let far = cam.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let ro = near.xyz / near.w;
    let rd = normalize(far.xyz / far.w - ro);

    var seed = (u32(in.pos.x) * 1973u) ^ (u32(in.pos.y) * 9277u) ^ (u32(rt.config.z) * 26699u);
    seed = seed | 1u;

    let SPP = 2;
    var radiance = vec3<f32>(0.0);
    for (var s = 0; s < SPP; s = s + 1) {
        // Clamp each sample to tame fireflies (rare very-bright paths that otherwise leave
        // sparkles the temporal average is slow to remove).
        radiance = radiance + min(trace_path(ro, rd, &seed), vec3<f32>(16.0));
    }
    return vec4<f32>(radiance / f32(SPP), 1.0);
}
