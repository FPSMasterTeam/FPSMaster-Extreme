// Screen-space ray-traced ambient occlusion.
//
// Reconstructs the world position (camera-relative, matching the TLAS space) and a
// geometric normal from the depth buffer, then casts a few short cosine-weighted
// hemisphere occlusion rays against the TLAS and writes a single-channel AO factor
// (1 = open, <1 = occluded). The per-pixel result is stochastic/noisy by design — it
// is spatially denoised (rt_denoise.wgsl) and multiplied onto the scene BEFORE the
// temporal upscaler, so the noise never reaches TAA/DLSS.
enable wgpu_ray_query;

// group(0): the RayTracer's shared bind group (TLAS + RtParams), the same layout the
// chunk RT pipelines bind at group(3).
@group(0) @binding(0) var tlas: acceleration_structure;
struct RtParams {
    // x: shadow tmax, y: AO tmax (blocks), z: frame counter, w: sun radius
    config: vec4<f32>,
    // x: shadow samples, y: AO samples, z: AO strength, w: unused
    quality: vec4<f32>,
};
@group(0) @binding(1) var<uniform> rt: RtParams;

// group(1): depth + camera for world-position reconstruction.
struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};
@group(1) @binding(0) var depth_tex: texture_2d<f32>;
@group(1) @binding(1) var<uniform> cam: PostCamera;
// Geometric normal G-buffer (0.5 + 0.5*n), written by the opaque chunk normal pass.
// Exact + stable at every angle, unlike a depth-derivative normal.
@group(1) @binding(2) var normal_tex: texture_2d<f32>;

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

fn world_from_uv(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let w = cam.inv_view_proj * clip;
    return w.xyz / w.w;
}

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}
fn rand(seed: ptr<function, u32>) -> f32 {
    *seed = pcg(*seed);
    return f32(*seed) * (1.0 / 4294967296.0);
}

// Branchless orthonormal basis (Duff et al.) with the third column = n.
fn basis(n: vec3<f32>) -> mat3x3<f32> {
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    let t = vec3<f32>(1.0 + s * n.x * n.x * a, s * b, -s * n.x);
    let bt = vec3<f32>(b, s + n.y * n.y * a, -n.y);
    return mat3x3<f32>(t, bt, n);
}

fn occluded(origin: vec3<f32>, dir: vec3<f32>, tmax: f32) -> bool {
    var rq: ray_query;
    // flags 0x04 = terminate on first hit (any-hit is enough for occlusion).
    rayQueryInitialize(&rq, tlas, RayDesc(0x04u, 0xFFu, 0.02, tmax, origin, dir));
    rayQueryProceed(&rq);
    return rayQueryGetCommittedIntersection(&rq).kind != 0u;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) f32 {
    let dims = vec2<f32>(textureDimensions(depth_tex));
    let px = vec2<i32>(clamp(in.uv * dims, vec2<f32>(0.0), dims - vec2<f32>(1.0)));
    let depth = textureLoad(depth_tex, px, 0).r;
    if (depth >= 1.0) {
        return 1.0; // sky / far plane: fully open
    }
    let samples = i32(rt.quality.y);
    if (samples <= 0) {
        return 1.0;
    }
    // Exact geometric normal from the G-buffer (decode 0.5+0.5*n). Near-zero length =
    // a pixel the opaque chunk normal pass didn't write (entities / no geometry) — skip
    // AO there rather than darken with a bogus normal.
    let n_enc = textureLoad(normal_tex, px, 0).xyz * 2.0 - 1.0;
    if (dot(n_enc, n_enc) < 0.25) {
        return 1.0;
    }
    let n = normalize(n_enc);
    let p = world_from_uv(in.uv, depth);
    let bn = basis(n);
    let tmax = rt.config.y;
    let strength = rt.quality.z;
    // Fixed per-pixel seed — NO frame counter. The AO is spatially denoised, so it must
    // be temporally STABLE: a per-frame random seed makes the AO change every frame,
    // which the spatial filter can't resolve and DLSS/TAA can't converge → flicker /
    // moiré when standing still. With a fixed seed the only per-frame change is the
    // sub-pixel jitter, which the temporal upscaler resolves.
    var seed = pcg(u32(in.pos.x) * 1973u + u32(in.pos.y) * 9277u + 1u);
    // Ray-origin bias grows with view distance to clear depth-reconstruction error +
    // the derivative-estimated normal, avoiding self-intersection stripes.
    let origin = p + n * (0.04 + length(p) * 0.0015);
    var occ = 0.0;
    for (var i = 0; i < samples; i = i + 1) {
        let u1 = rand(&seed);
        let u2 = rand(&seed);
        // Cosine-weighted hemisphere direction.
        let r = sqrt(u1);
        let a = 6.2831853 * u2;
        let local = vec3<f32>(r * cos(a), r * sin(a), sqrt(max(0.0, 1.0 - u1)));
        let dir = bn * local;
        if (occluded(origin, dir, tmax)) {
            occ = occ + 1.0;
        }
    }
    return clamp(1.0 - (occ / f32(samples)) * strength, 0.0, 1.0);
}
