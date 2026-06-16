// Fullscreen sky gradient + 2D procedural cloud layer. The view ray is rebuilt
// per pixel from the inverse rotation-only view-projection. Clouds are a flat
// layer sampled directly here (no raymarch, no half-res buffer) — full-res and
// noise-free, with fake sun-relief shading for a sense of volume.

struct Sky {
    inv_view_proj: mat4x4<f32>,
    horizon: vec4<f32>,
    zenith: vec4<f32>,
    // xyz = world-space sun direction, w = sunset glow strength.
    sun_dir: vec4<f32>,
    sunset: vec4<f32>,
    // xyz = camera world position, w = time (seconds).
    camera_pos: vec4<f32>,
    // x = clouds enabled, y = coverage, z = density, w = day factor.
    cloud_params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> sky: Sky;

// Tileable 3D noise (R = base shape, G = detail), sampled at fixed slices for the
// flat cloud layer.
@group(1) @binding(0)
var noise_tex: texture_3d<f32>;
@group(1) @binding(1)
var noise_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let pos = positions[vertex_index];
    var out: VertexOutput;
    out.clip_position = vec4<f32>(pos, 1.0, 1.0);
    out.ndc = pos;
    return out;
}

// Cloud coverage 0..1 at a point on the cloud plane (noise-space uv).
fn cloud_density(uv: vec2<f32>) -> f32 {
    let base = textureSampleLevel(noise_tex, noise_sampler, vec3<f32>(uv, 0.25), 0.0).r;
    let cov = sky.cloud_params.y;
    var d = smoothstep(1.0 - cov - 0.18, 1.0 - cov + 0.12, base);
    // Erode edges with higher-frequency detail.
    let det = textureSampleLevel(noise_tex, noise_sampler, vec3<f32>(uv * 3.0, 0.6), 0.0).g;
    d = clamp(d - (1.0 - det) * 0.45, 0.0, 1.0);
    return d * sky.cloud_params.z;
}

// Flat cloud layer with fake sun-relief shading. Returns (rgb, coverage).
fn clouds(dir: vec3<f32>) -> vec4<f32> {
    let fade = smoothstep(0.04, 0.22, dir.y);
    if (fade <= 0.0) {
        return vec4<f32>(0.0);
    }
    // Intersect a flat layer above the camera; flat-plane perspective makes the
    // clouds converge toward the horizon naturally.
    let height = 220.0;
    let t = height / max(dir.y, 0.04);
    let wxz = sky.camera_pos.xz + dir.xz * t;
    let wind = vec2<f32>(sky.camera_pos.w * 1.5, sky.camera_pos.w * 0.5);
    let uv = (wxz + wind) * 0.0016;

    let d = cloud_density(uv);
    if (d <= 0.001) {
        return vec4<f32>(0.0);
    }
    // Relief: compare density a short way toward the sun (in the cloud plane). A
    // higher value here than sunward = a sunlit bump; lower = self-shadowed.
    let sun_xz = normalize(sky.sun_dir.xz + vec2<f32>(1e-4, 0.0));
    let relief = d - cloud_density(uv + sun_xz * 0.025);
    let day = max(sky.cloud_params.w, 0.0);
    let shadow_col = mix(vec3<f32>(0.34, 0.36, 0.42), vec3<f32>(0.60, 0.63, 0.70), day);
    let lit_col = mix(vec3<f32>(0.5, 0.5, 0.55), vec3<f32>(1.0, 0.98, 0.93), day);
    var col = mix(shadow_col, lit_col, smoothstep(-0.15, 0.25, relief));
    // Warm silver lining on sun-facing edges.
    col = col + vec3<f32>(1.0, 0.95, 0.82) * max(relief, 0.0) * 1.3 * day;
    return vec4<f32>(col, d * fade);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let world = sky.inv_view_proj * vec4<f32>(input.ndc, 1.0, 1.0);
    let dir = normalize(world.xyz / world.w);

    let t = smoothstep(0.0, 0.55, clamp(dir.y, 0.0, 1.0));
    var color = mix(sky.horizon.rgb, sky.zenith.rgb, t);

    let glow = sky.sun_dir.w;
    if (glow > 0.0) {
        let toward = max(dot(dir, sky.sun_dir.xyz), 0.0);
        let band = 1.0 - smoothstep(0.0, 0.35, abs(dir.y));
        let amount = clamp(pow(toward, 4.0) * band * glow, 0.0, 1.0);
        color = mix(color, sky.sunset.rgb, amount);
    }

    if (sky.cloud_params.x > 0.5) {
        let c = clouds(dir);
        color = mix(color, c.rgb, c.a);
    }

    return vec4<f32>(color, 1.0);
}
