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
// PBR (labPBR) atlases: normal (RGB tangent-space normal) + specular
// (R smoothness, G metallic, B emissive). Default 1×1 textures when no PBR pack, so
// the tracer degrades to plain diffuse. Sampled with the linear PBR sampler.
@group(1) @binding(4) var normal_atlas: texture_2d<f32>;
@group(1) @binding(5) var specular_atlas: texture_2d<f32>;
@group(1) @binding(6) var pbr_samp: sampler;

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

// ── PBR (labPBR) material + GGX BRDF, mirroring the raster chunk shader ──
fn ggx_distribution(ndoth: f32, rough: f32) -> f32 {
    let a = rough * rough;
    let a2 = a * a;
    let d = ndoth * ndoth * (a2 - 1.0) + 1.0;
    return a2 / max(3.14159265 * d * d, 1e-7);
}
fn smith_ggx(ndotv: f32, ndotl: f32, rough: f32) -> f32 {
    let r = rough + 1.0;
    let k = (r * r) / 8.0;
    let gv = ndotv / (ndotv * (1.0 - k) + k);
    let gl = ndotl / (ndotl * (1.0 - k) + k);
    return gv * gl;
}
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - cos_theta, 5.0);
}
// labPBR tangent-space normal → world, TBN from the axis-aligned face normal (mirrors
// the raster `sample_pbr_normal`).
fn pbr_normal(uv: vec2<f32>, geo_n: vec3<f32>) -> vec3<f32> {
    let n_tex = textureSampleLevel(normal_atlas, pbr_samp, uv, 0.0);
    var ts = n_tex.rgb * 2.0 - vec3<f32>(1.0);
    ts.x = ts.x * 1.5;
    ts.y = ts.y * 1.5;
    let abs_n = abs(geo_n);
    var tangent: vec3<f32>;
    var bitangent: vec3<f32>;
    if (abs_n.y > 0.9) {
        tangent = vec3<f32>(1.0, 0.0, 0.0);
        bitangent = vec3<f32>(0.0, 0.0, sign(geo_n.y));
    } else if (abs_n.x > 0.9) {
        tangent = vec3<f32>(0.0, 0.0, -sign(geo_n.x));
        bitangent = vec3<f32>(0.0, 1.0, 0.0);
    } else {
        tangent = vec3<f32>(sign(geo_n.z), 0.0, 0.0);
        bitangent = vec3<f32>(0.0, 1.0, 0.0);
    }
    return normalize(tangent * ts.x + bitangent * ts.y + geo_n * ts.z);
}
// Combined diffuse + GGX specular BRDF (no cosine; caller multiplies by N·L). Matches
// the raster: kd*albedo + D*G*F/(4 N·V N·L), kd = (1-F)(1-metallic).
fn brdf_eval(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, albedo: vec3<f32>, f0: vec3<f32>, rough: f32, metallic: f32) -> vec3<f32> {
    let h = normalize(v + l);
    let ndotv = max(dot(n, v), 0.001);
    let ndotl = max(dot(n, l), 0.0);
    let ndoth = max(dot(n, h), 0.0);
    let vdoth = max(dot(v, h), 0.0);
    let dterm = ggx_distribution(ndoth, rough);
    let gterm = smith_ggx(ndotv, ndotl, rough);
    let fterm = fresnel_schlick(vdoth, f0);
    let spec = (dterm * gterm * fterm) / max(4.0 * ndotv * ndotl, 0.001);
    let kd = (vec3<f32>(1.0) - fterm) * (1.0 - metallic);
    return kd * albedo + spec;
}
// Importance-sample a GGX half-vector around n and reflect the view → bounce direction.
fn sample_ggx_dir(n: vec3<f32>, v: vec3<f32>, rough: f32, seed: ptr<function, u32>) -> vec3<f32> {
    let a = rough * rough;
    let r1 = rand(seed);
    let r2 = rand(seed);
    let phi = 6.2831853 * r1;
    let cos_t = sqrt((1.0 - r2) / (1.0 + (a * a - 1.0) * r2));
    let sin_t = sqrt(max(0.0, 1.0 - cos_t * cos_t));
    var up = vec3<f32>(0.0, 1.0, 0.0);
    if (abs(n.y) > 0.99) { up = vec3<f32>(1.0, 0.0, 0.0); }
    let tx = normalize(cross(up, n));
    let ty = cross(n, tx);
    let h = normalize(tx * (sin_t * cos(phi)) + ty * (sin_t * sin(phi)) + n * cos_t);
    return reflect(-v, h);
}

fn occluded(origin: vec3<f32>, dir: vec3<f32>, tmax: f32) -> bool {
    var rq: ray_query;
    rayQueryInitialize(&rq, tlas, RayDesc(0x04u, 0x01u, 0.02, tmax, origin, dir));
    rayQueryProceed(&rq);
    return rayQueryGetCommittedIntersection(&rq).kind != 0u;
}

const MAX_BOUNCES: i32 = 3;

struct PtResult {
    radiance: vec3<f32>,
    // 1.0 if the primary ray's first hit is a transparent surface (water/glass), else 0.
    // The denoiser uses it to composite: keep the PT result for water/glass, but keep the
    // rasterized entities/hand (which the PT can't trace) where it's 0 yet there's no chunk.
    transparent: f32,
};

fn trace_path(ro: vec3<f32>, rd: vec3<f32>, seed: ptr<function, u32>) -> PtResult {
    var origin = ro;
    var dir = rd;
    var throughput = vec3<f32>(1.0);
    var radiance = vec3<f32>(0.0);
    var transparent = 0.0;
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
        let n_geo = unpack_normal(tri_normals[idx]);
        // Front face = the ray hit the outward-facing side. For water/glass (outward
        // normals) this is the air→medium entry; a back-face hit is the medium→air exit.
        let front_face = dot(dir, n_geo) < 0.0;
        var n = n_geo;
        if (!front_face) { n = -n; } // flip to face the ray
        let pos = origin + dir * it.t;
        let is_glass = ((packed >> 25u) & 1u) == 1u;
        let is_water = ((packed >> 26u) & 1u) == 1u;
        let coverage = f32((packed >> 27u) & 0x1Fu) / 31.0;
        // Mark this pixel as transparent if the FIRST surface the primary ray hits is
        // water/glass (so the denoiser keeps the PT result here, not the rasterized scene).
        if (iter == 0 && (is_glass || is_water)) {
            transparent = 1.0;
        }
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
            // Tint on entry only — a glass block is two faces (front + back), so tinting
            // both would square the colour (too dark when looking through glass).
            if (front_face) {
                throughput = throughput * mix(vec3<f32>(1.0), albedo, texel.a);
            }
            origin = pos + dir * 0.002;
            continue;
        }
        // Water: Fresnel — stochastically reflect (sky/scene) or REFRACT (bend by the
        // water IOR) and tint by the water colour so the body is visibly coloured. More
        // water crossed = more tint (Beer-Lambert-like). Neither counts as a bounce.
        if (is_water) {
            // IOR by side: entering air→water (η 0.75), exiting water→air (η 1.333, which
            // total-internal-reflects at grazing — the silvery underside of the surface).
            // Using a fixed 0.75 for every face was the bug: exits bent wrong and the tint
            // stacked. Tint only on ENTRY so a ray crossing several water faces (enter +
            // exit, or adjacent volumes) doesn't over-darken.
            let eta = select(1.333, 0.75, front_face);
            let fres = 0.02 + 0.98 * pow(1.0 - abs(dot(n, dir)), 5.0);
            if (rand(seed) < fres) {
                dir = reflect(dir, n);
                origin = pos + n * 0.02;
            } else {
                let refr = refract(dir, n, eta);
                if (dot(refr, refr) < 1e-6) {
                    dir = reflect(dir, n); // total internal reflection
                    origin = pos + n * 0.02;
                } else {
                    dir = normalize(refr);
                    if (front_face) {
                        throughput = throughput * vec3<f32>(0.35, 0.62, 0.78);
                    }
                    origin = pos + dir * 0.01;
                }
            }
            continue;
        }

        // ── Opaque PBR surface ──
        // `n` is the geometric face normal (flipped to face the ray); use it for the TBN
        // and the ray offset. The shading normal comes from the labPBR normal map.
        let geo_n = n;
        var sn = pbr_normal(uv, geo_n);
        if (dot(sn, dir) > 0.0) { sn = geo_n; } // keep the shading normal facing the ray
        let spec_tex = textureSampleLevel(specular_atlas, pbr_samp, uv, 0.0);
        let smoothness = spec_tex.r;
        let metallic = spec_tex.g;
        let roughness = max((1.0 - smoothness) * (1.0 - smoothness), 0.04);
        let f0 = mix(vec3<f32>(0.04), albedo, metallic);
        let diff_albedo = albedo * (1.0 - metallic); // metals have no diffuse term
        let v = -dir;                                 // view (toward the incoming ray)
        let surf = pos + geo_n * 0.02;

        // Emission: the full-emissive block flag (glowstone/lava) OR the labPBR emissive
        // (specular B). First opaque hit only — indirect emitter light is the point-light NEE.
        let emit = select(spec_tex.b, 1.0, ((packed >> 24u) & 1u) == 1u);
        if (bounces == 0 && emit > 0.01) {
            radiance = radiance + throughput * albedo * emit * 3.0;
        }
        // NEE: sun, full BRDF (diffuse + GGX specular highlight).
        let ndl = max(dot(sn, sun), 0.0);
        if (ndl > 0.0 && day > 0.0 && !occluded(surf, sun, 1000.0)) {
            let b = brdf_eval(sn, v, sun, albedo, f0, roughness, metallic);
            radiance = radiance + throughput * b * sun_color * ndl;
        }
        // NEE: emissive-block point lights, full BRDF.
        let lcount = arrayLength(&pt_lights);
        for (var i = 0u; i < lcount; i = i + 1u) {
            let L = pt_lights[i];
            if (L.color.w <= 0.0) { continue; }
            let to = L.pos_radius.xyz - pos;
            let ld = length(to);
            if (ld >= L.pos_radius.w) { continue; }
            let ldir = to / max(ld, 1e-4);
            let lndl = max(dot(sn, ldir), 0.0);
            if (lndl <= 0.0) { continue; }
            let r = ld / L.pos_radius.w;
            let win = clamp(1.0 - r * r * r * r, 0.0, 1.0);
            let atten = win / (1.0 + ld * ld * 0.2);
            if (!occluded(surf, ldir, ld - 0.7)) {
                let b = brdf_eval(sn, v, ldir, albedo, f0, roughness, metallic);
                radiance = radiance + throughput * b * L.color.rgb * (L.color.w * atten * lndl);
            }
        }
        // Diffuse ambient = a sky-independent floor + a sky-visibility term (metals get
        // none — their fill is the spec bounce). The FLOOR lifts deep shadow / occluded
        // faces (it barely touches already-bright surfaces, which ACES compresses); the SKY
        // term is the open-area fill. The user Brightness slider (sky.sunset.w, 0..1, default
        // 0.5) scales the whole ambient 0.4..1.6× so "how dark the dark parts get" is
        // dial-able in-game.
        let bright = 0.4 + 1.2 * sky.sunset.w;
        let ambient = (vec3<f32>(0.09, 0.1, 0.13) + sky_color(sn) * 0.16) * bright;
        radiance = radiance + throughput * diff_albedo * ambient;

        // ── Bounce: stochastically pick the diffuse or the GGX specular lobe. The specular
        // lobe IS the reflection — sharp for smooth/metallic, blurry for rough — so polished
        // and metal blocks reflect the scene. One sample → noisy; the temporal resolve /
        // denoiser cleans it up. ──
        if (bounces >= MAX_BOUNCES) { break; }
        bounces = bounces + 1;
        let fres_v = fresnel_schlick(max(dot(sn, v), 0.0), f0);
        // Lower floor so rough dielectrics don't over-sample (and over-brighten) specular.
        let p_spec = clamp(max(fres_v.r, max(fres_v.g, fres_v.b)), 0.03, 0.9);
        if (rand(seed) < p_spec) {
            let l = sample_ggx_dir(sn, v, roughness, seed);
            if (dot(l, geo_n) <= 0.0) { break; } // sampled below the surface
            let h = normalize(v + l);
            let ndotv = max(dot(sn, v), 1e-3);
            let ndoth = max(dot(sn, h), 1e-3);
            let vdoth = max(dot(v, h), 1e-3);
            let g = smith_ggx(ndotv, max(dot(sn, l), 1e-3), roughness);
            let f = fresnel_schlick(vdoth, f0);
            // BRDF·cos / pdf — D and N·H cancel in the GGX estimator. CLAMP it: the 1/N·V
            // term explodes at grazing angles (block silhouettes) → bright fireflies the
            // denoiser then smears into a halo ring. The 0.5 tones the mirror down to match
            // the raster's damped reflections (a full reflection reads as too strong).
            let w = min(f * g * vdoth / (ndotv * ndoth), vec3<f32>(4.0)) * 0.5;
            throughput = throughput * w / p_spec;
            dir = l;
        } else {
            throughput = throughput * diff_albedo / (1.0 - p_spec);
            dir = cosine_dir(sn, seed);
        }
        origin = surf;
        if (max(throughput.r, max(throughput.g, throughput.b)) < 0.02) { break; }
    }
    return PtResult(radiance, transparent);
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
    var transparent = 0.0;
    for (var s = 0; s < SPP; s = s + 1) {
        // Clamp each sample to tame fireflies (rare very-bright paths that otherwise leave
        // sparkles the temporal average is slow to remove).
        let r = trace_path(ro, rd, &seed);
        radiance = radiance + min(r.radiance, vec3<f32>(16.0));
        transparent = r.transparent; // deterministic across samples (primary hit is fixed)
    }
    // a = the transparent (water/glass) flag, consumed by the denoiser's compositing.
    return vec4<f32>(radiance / f32(SPP), transparent);
}
