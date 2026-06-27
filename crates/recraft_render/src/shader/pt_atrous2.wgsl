// Second à-trous iteration of the demodulated path-traced irradiance (SVGF multi-pass).
// Pass 1 (pt_denoise) wrote the temporally-accumulated, once-filtered irradiance to an
// intermediate (filt_tex.rgb) plus a transparent flag (filt_tex.a). This pass widens the
// filter (larger step → reaches noise the tight first pass left in deep shadow / weak
// light), then remodulates by the per-pixel albedo and composites: sky passes through,
// entity / hand pixels (empty albedo, not transparent) are discarded so the rasterized
// `view` shows through, water/glass keep their sharp (unfiltered) radiance.
struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};
@group(0) @binding(0) var filt_tex: texture_2d<f32>;   // pass-1 irradiance (+ .a transparent)
@group(0) @binding(1) var albedo_tex: texture_2d<f32>; // primary-surface albedo G-buffer
@group(0) @binding(2) var depth_tex: texture_2d<f32>;  // scene depth (non-linear)
@group(0) @binding(3) var<uniform> cam: PostCamera;

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

fn world_at(px: vec2<i32>, dims: vec2<i32>, depth: f32) -> vec3<f32> {
    let uv = (vec2<f32>(px) + vec2<f32>(0.5)) / vec2<f32>(dims);
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let w = cam.inv_view_proj * clip;
    return w.xyz / w.w;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(filt_tex));
    let c = vec2<i32>(clamp(in.uv * vec2<f32>(dims), vec2<f32>(0.0), vec2<f32>(dims) - vec2<f32>(1.0)));
    let f = textureLoad(filt_tex, c, 0);
    let cd = textureLoad(depth_tex, c, 0).r;
    if (cd >= 1.0) {
        return vec4<f32>(f.rgb, 1.0); // sky — pass through
    }
    let albedo4 = textureLoad(albedo_tex, c, 0);
    // Empty albedo + not a transparent (water/glass) surface → rasterized entity / hand.
    if (albedo4.a < 0.5 && f.a < 0.5) {
        discard;
    }
    let ca = select(vec3<f32>(1.0), max(albedo4.rgb, vec3<f32>(0.04)), albedo4.a >= 0.5);
    // Water/glass (empty albedo, transparent) keep their sharp radiance — no second blur.
    if (albedo4.a < 0.5) {
        return vec4<f32>(f.rgb * ca, 1.0);
    }
    let ci = f.rgb;
    let cl = dot(ci, vec3<f32>(0.299, 0.587, 0.114));
    let cp = world_at(c, dims, cd);

    // Wider à-trous (step 4): 5×5 taps reach ±8 px on the once-filtered irradiance.
    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    let step = 4;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let p = clamp(c + vec2<i32>(dx, dy) * step, vec2<i32>(0), dims - vec2<i32>(1));
            let d = textureLoad(depth_tex, p, 0).r;
            if (d >= 1.0) {
                continue;
            }
            let wp = world_at(p, dims, d);
            let diff = wp - cp;
            let dw = exp(-dot(diff, diff) * 4.0);
            let r2 = f32(dx * dx + dy * dy);
            let sw = exp(-r2 * 0.25);
            let ip = textureLoad(filt_tex, p, 0).rgb;
            let il = dot(ip, vec3<f32>(0.299, 0.587, 0.114));
            let lw = exp(-abs(il - cl) / (cl * 0.6 + 0.1));
            let w = dw * sw * lw;
            sum = sum + ip * w;
            wsum = wsum + w;
        }
    }
    let filt = select(ci, sum / wsum, wsum > 0.0);
    return vec4<f32>(filt * ca, 1.0); // remodulate with this pixel's sharp albedo
}
