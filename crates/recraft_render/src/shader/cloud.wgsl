// Half-resolution volumetric clouds. Raymarches a cloud slab using a precomputed
// tileable 3D noise texture (base shape + detail erosion), with Henyey-Greenstein
// forward scattering and a Beer + powder term for the silver lining. Output is
// (scattered.rgb, coverage.a); the sky pass upsamples and composites it.

struct Sky {
    inv_view_proj: mat4x4<f32>,
    horizon: vec4<f32>,
    zenith: vec4<f32>,
    sun_dir: vec4<f32>,   // xyz dir to sun, w sunset glow
    sunset: vec4<f32>,
    camera_pos: vec4<f32>, // xyz cam, w time (s)
    cloud_params: vec4<f32>, // x enabled, y coverage, z density, w day factor
};
@group(0) @binding(0) var<uniform> sky: Sky;
@group(1) @binding(0) var noise_tex: texture_3d<f32>;
@group(1) @binding(1) var noise_sampler: sampler;

const BOTTOM: f32 = 140.0;
const TOP: f32 = 360.0;
const BASE_SCALE: f32 = 0.00045;
const DETAIL_SCALE: f32 = 0.0032;

// Remap v from [lo,hi] to [0,1].
fn remap01(v: f32, lo: f32, hi: f32) -> f32 {
    return clamp((v - lo) / max(hi - lo, 0.0001), 0.0, 1.0);
}

// Cloud density at a world position: base shape (coverage-thresholded) eroded by
// detail, faded across the slab thickness for soft tops/bottoms.
fn density(pos: vec3<f32>, wind: vec3<f32>) -> f32 {
    let h = clamp((pos.y - BOTTOM) / (TOP - BOTTOM), 0.0, 1.0);
    let base = textureSampleLevel(noise_tex, noise_sampler, pos * BASE_SCALE + wind, 0.0).r;
    // Coverage: higher cloud_params.y → more sky filled.
    let cov = sky.cloud_params.y;
    var d = remap01(base, 1.0 - cov, 1.0);
    // Vertical profile: rounded bottom, soft anvil top.
    d = d * smoothstep(0.0, 0.18, h) * smoothstep(1.0, 0.55, h);
    if (d <= 0.0) {
        return 0.0;
    }
    // Erode edges with higher-frequency detail.
    let det = textureSampleLevel(noise_tex, noise_sampler, pos * DETAIL_SCALE + wind * 2.0, 0.0).g;
    d = remap01(d, det * 0.55, 1.0);
    return d * sky.cloud_params.z;
}

// Henyey-Greenstein phase for forward scattering.
fn hg(cos_t: f32, g: f32) -> f32 {
    let g2 = g * g;
    return (1.0 - g2) / (4.0 * 3.14159 * pow(1.0 + g2 - 2.0 * g * cos_t, 1.5));
}

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VsOut;
    out.clip_position = vec4<f32>(p[vi], 1.0, 1.0);
    out.ndc = p[vi];
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let world = sky.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = normalize(world.xyz / world.w);
    // Soft horizon: fade out toward eye level instead of a hard clip.
    let horizon_fade = smoothstep(0.0, 0.12, dir.y);
    if (horizon_fade <= 0.0) {
        return vec4<f32>(0.0);
    }

    let ro = sky.camera_pos.xyz;
    let time = sky.camera_pos.w;
    let wind = vec3<f32>(time * 0.006, time * 0.001, time * 0.002);
    let t0 = (BOTTOM - ro.y) / dir.y;
    let t1 = (TOP - ro.y) / dir.y;
    if (t1 <= 0.0) {
        return vec4<f32>(0.0);
    }
    let start = max(t0, 0.0);

    // Adaptive step count: fewer at grazing angles (very long ray) to bound cost.
    let span = t1 - start;
    let steps = i32(clamp(span / 14.0, 24.0, 64.0));
    let dt = span / f32(steps);

    let sun = sky.sun_dir.xyz;
    let day = sky.cloud_params.w;
    let sun_col = vec3<f32>(1.0, 0.96, 0.88) * (0.7 + 0.6 * day);
    let ambient = (sky.zenith.rgb * 0.5 + sky.horizon.rgb * 0.3) + vec3<f32>(0.05);
    let cos_t = dot(dir, sun);
    let phase = mix(hg(cos_t, 0.2), hg(cos_t, -0.15), 0.5);

    var transmittance = 1.0;
    var scattered = vec3<f32>(0.0);
    for (var i = 0; i < steps; i = i + 1) {
        let pos = ro + dir * (start + dt * (f32(i) + 0.5));
        let d = density(pos, wind);
        if (d > 0.01) {
            // Light march toward the sun (a few cheap samples) → Beer transmittance.
            var ld = 0.0;
            let lstep = 26.0;
            for (var j = 0; j < 4; j = j + 1) {
                ld = ld + density(pos + sun * (lstep * (f32(j) + 0.5)), wind);
            }
            let sun_t = exp(-ld * lstep * 0.06);
            // Powder term: darkens cloud cores, brightens lit edges (silver lining).
            let powder = 1.0 - exp(-d * dt * 0.6);
            let lit = ambient + sun_col * (sun_t * phase * 8.0 + 0.15) * powder;
            let absorb = d * dt * 0.08;
            scattered = scattered + transmittance * lit * absorb;
            transmittance = transmittance * exp(-absorb);
            if (transmittance < 0.02) {
                break;
            }
        }
    }
    let coverage = (1.0 - transmittance) * horizon_fade;
    return vec4<f32>(scattered * horizon_fade, coverage);
}
