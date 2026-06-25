// Ray-traced variant of the volumetric light pass (god rays). Identical raymarch to
// volumetric.wgsl, but each step's sun visibility is a hardware ray-query against the
// TLAS instead of a shadow-map lookup — so shafts aren't bounded by the shadow map's
// resolution or coverage (no peter-panning, accurate far from the camera). The march
// already runs in camera-relative space (inv_view_proj = view_proj_rel.inverse()),
// which is exactly the TLAS space, so march points are ray origins directly.
enable wgpu_ray_query;

struct Vol {
    inv_view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    sun_dir: vec4<f32>,    // xyz: world-space direction TO the sun (normalized)
    sun_color: vec4<f32>,  // rgb: sun intensity (already day/night scaled)
    camera_pos: vec4<f32>, // xyz: camera-relative eye, w: day factor (sky_brightness)
    params: vec4<f32>,     // x: density, y: max distance, z: HG g, w: intensity
};

// group(0): reuses the shadow-map pass's bind group; only depth (0) + the uniform (3)
// are read here (shadow_map / sampler at 1,2 are left unbound-by-use).
@group(0) @binding(0) var depth_tex: texture_2d<f32>;
@group(0) @binding(3) var<uniform> vol: Vol;
// group(1): the RayTracer's TLAS+params bind group (only the TLAS is used).
@group(1) @binding(0) var rt_tlas: acceleration_structure;

const STEPS: i32 = 20;
const PI: f32 = 3.14159265;

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

fn world_from_depth(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let w = vol.inv_view_proj * clip;
    return w.xyz / w.w;
}

fn load_depth(uv: vec2<f32>) -> f32 {
    let dims = vec2<f32>(textureDimensions(depth_tex));
    let px = vec2<i32>(clamp(uv * dims, vec2<f32>(0.0), dims - 1.0));
    return textureLoad(depth_tex, px, 0).r;
}

fn bayer(p: vec2<i32>) -> f32 {
    var m = array<i32, 16>(0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5);
    let i = (p.y & 3) * 4 + (p.x & 3);
    return f32(m[i]) / 16.0;
}

fn henyey_greenstein(cos_t: f32, g: f32) -> f32 {
    let g2 = g * g;
    return (1.0 - g2) / (4.0 * PI * pow(max(1.0 + g2 - 2.0 * g * cos_t, 1e-4), 1.5));
}

// Sun visibility via a hardware ray toward the sun: 1 = lit, 0 = occluded. `p` is
// camera-relative (= TLAS space). Capped at the TLAS range (RT_RANGE_BLOCKS).
fn sun_lit(p: vec3<f32>) -> f32 {
    var rq: ray_query;
    rayQueryInitialize(
        &rq,
        rt_tlas,
        RayDesc(0x04u, 0x01u, 0.05, 160.0, p, vol.sun_dir.xyz),
    );
    rayQueryProceed(&rq);
    return select(1.0, 0.0, rayQueryGetCommittedIntersection(&rq).kind != 0u);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let day = vol.camera_pos.w;
    if (day <= 0.02) {
        return vec4<f32>(0.0);
    }

    let cam = vol.camera_pos.xyz;
    let depth = load_depth(in.uv);
    let scene = world_from_depth(in.uv, depth);
    let to_scene = scene - cam;
    let rd = normalize(to_scene);
    var march_len = length(to_scene);
    if (depth >= 0.9999) {
        march_len = vol.params.y;
    }
    march_len = min(march_len, vol.params.y);

    let sigma = vol.params.x;
    let step_len = march_len / f32(STEPS);
    let phase = henyey_greenstein(dot(rd, vol.sun_dir.xyz), vol.params.z);

    var t = step_len * bayer(vec2<i32>(in.pos.xy));
    var transmittance = 1.0;
    var accum = 0.0;
    for (var i = 0; i < STEPS; i = i + 1) {
        let p = cam + rd * t;
        let lit = sun_lit(p);
        accum = accum + lit * transmittance * sigma * step_len;
        transmittance = transmittance * exp(-sigma * step_len);
        t = t + step_len;
    }

    let col = vol.sun_color.rgb * (accum * phase * vol.params.w * day);
    return vec4<f32>(col, 1.0);
}
