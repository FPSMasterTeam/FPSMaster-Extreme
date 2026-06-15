// Volumetric light ("体积光" / god rays / 丁达尔效应). Raymarch the view ray from
// the camera to the scene surface (read from the world depth buffer), sampling the
// sun shadow map at each step. Where a sample sits in sunlight it scatters light
// toward the eye, weighted by a Henyey-Greenstein phase toward the sun — so shafts
// brighten near the sun and fade away from it, and gaps in geometry (windows,
// leaves, cave mouths) carve dark slots through the haze. Rendered at half
// resolution into an HDR target, then added to the scene in the post pass.

struct Vol {
    inv_view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    sun_dir: vec4<f32>,    // xyz: world-space direction TO the sun (normalized)
    sun_color: vec4<f32>,  // rgb: sun intensity (already day/night scaled)
    camera_pos: vec4<f32>, // xyz: world-space eye, w: day factor (sky_brightness)
    params: vec4<f32>,     // x: density, y: max distance, z: HG g, w: intensity
};

@group(0) @binding(0) var depth_tex: texture_depth_2d;
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;
@group(0) @binding(3) var<uniform> vol: Vol;

const STEPS: i32 = 32;
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

// World position from a screen UV + non-linear depth.
fn world_from_depth(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let w = vol.inv_view_proj * clip;
    return w.xyz / w.w;
}

fn load_depth(uv: vec2<f32>) -> f32 {
    let dims = vec2<f32>(textureDimensions(depth_tex));
    let px = vec2<i32>(clamp(uv * dims, vec2<f32>(0.0), dims - 1.0));
    return textureLoad(depth_tex, px, 0);
}

// 4x4 Bayer dither in [0,1): offsets each pixel's first sample so the coarse step
// count reads as smooth haze instead of concentric banding.
fn bayer(p: vec2<i32>) -> f32 {
    var m = array<i32, 16>(0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5);
    let i = (p.y & 3) * 4 + (p.x & 3);
    return f32(m[i]) / 16.0;
}

// Henyey-Greenstein phase function (g>0 = forward scattering toward the sun).
fn henyey_greenstein(cos_t: f32, g: f32) -> f32 {
    let g2 = g * g;
    return (1.0 - g2) / (4.0 * PI * pow(max(1.0 + g2 - 2.0 * g * cos_t, 1e-4), 1.5));
}

// Sun visibility at a world point: 1 = lit, 0 = shadowed. Outside the shadow
// map's coverage everything counts as lit.
fn sun_lit(world_pos: vec3<f32>) -> f32 {
    let lc = vol.light_view_proj * vec4<f32>(world_pos, 1.0);
    let ndc = lc.xyz / lc.w;
    if (ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z > 1.0) {
        return 1.0;
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
    return textureSampleCompareLevel(shadow_map, shadow_sampler, uv, ndc.z - 0.0008);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let day = vol.camera_pos.w;
    // No sun below the horizon -> no shafts (avoids a night-time haze).
    if (day <= 0.02) {
        return vec4<f32>(0.0);
    }

    let cam = vol.camera_pos.xyz;
    let depth = load_depth(in.uv);
    let scene = world_from_depth(in.uv, depth);
    let to_scene = scene - cam;
    let rd = normalize(to_scene);
    // Sky pixels (depth at the far plane) march the full configured distance.
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
