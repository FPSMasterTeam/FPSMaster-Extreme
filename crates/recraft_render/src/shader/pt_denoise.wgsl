// Demodulated temporal+spatial denoise of the path-traced scene (SVGF-lite).
//
// The path tracer's noise is in the LIGHTING, not the texture, so everything runs in
// DEMODULATED irradiance (radiance / primary-surface albedo): the texture stays razor
// sharp because only the smooth lighting is filtered, then the per-pixel albedo is
// multiplied back. Two stages:
//   1. Spatial: an à-trous bilateral weighted by world-space distance + luminance.
//   2. Temporal: reproject last frame's irradiance through the (camera) motion vectors
//      and accumulate, with a current-neighbourhood colour clamp to reject ghosting on
//      disocclusion / fast motion. This is what gives clean lighting WITHOUT a heavy
//      spatial blur, so AO / contact shadows / reflections keep their detail.
// Sky pixels pass through; entity / hand pixels (empty albedo) are discarded so the
// rasterized `view` (loaded) shows through. Output: @0 the displayed colour, @1 the
// accumulated irradiance for next frame's history.
struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};
@group(0) @binding(0) var color_tex: texture_2d<f32>;  // noisy path-traced radiance
@group(0) @binding(1) var albedo_tex: texture_2d<f32>; // primary-surface albedo G-buffer
@group(0) @binding(2) var depth_tex: texture_2d<f32>;  // scene depth (non-linear)
@group(0) @binding(3) var<uniform> cam: PostCamera;
@group(0) @binding(4) var mv_tex: texture_2d<f32>;     // motion vectors (cur_uv - prev_uv)
@group(0) @binding(5) var hist_tex: texture_2d<f32>;   // last frame's accumulated irradiance
@group(0) @binding(6) var hist_samp: sampler;          // linear, for reprojection

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct Out {
    @location(0) view: vec4<f32>,  // displayed colour (irradiance × albedo)
    @location(1) hist: vec4<f32>,  // accumulated irradiance → next frame's history
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

// Demodulated irradiance: chunks divide by their albedo (so texture stays sharp); pixels
// with no opaque-chunk albedo (water / glass, mostly specular) are denoised in radiance
// space directly (ca = 1, no demodulation).
fn irradiance_at(p: vec2<i32>) -> vec3<f32> {
    let a = textureLoad(albedo_tex, p, 0);
    let ca = select(vec3<f32>(1.0), max(a.rgb, vec3<f32>(0.04)), a.a >= 0.5);
    return textureLoad(color_tex, p, 0).rgb / ca;
}

@fragment
fn fs_main(in: VsOut) -> Out {
    let dims = vec2<i32>(textureDimensions(color_tex));
    let c = vec2<i32>(clamp(in.uv * vec2<f32>(dims), vec2<f32>(0.0), vec2<f32>(dims) - vec2<f32>(1.0)));
    let pt = textureLoad(color_tex, c, 0);
    let center_color = pt.rgb;
    let transparent = pt.a; // 1 = the primary PT ray hit water/glass at this pixel
    let cd = textureLoad(depth_tex, c, 0).r;
    var o: Out;
    if (cd >= 1.0) {
        o.view = vec4<f32>(center_color, 1.0); // sky — PT result, no surface
        o.hist = vec4<f32>(0.0);
        return o;
    }
    let albedo4 = textureLoad(albedo_tex, c, 0);
    // No opaque chunk here AND the PT didn't hit a transparent surface → a rasterized
    // entity / hand the PT can't trace: discard, keep the rasterized `view`. Water/glass
    // (transparent set) and chunks keep the PT result.
    if (albedo4.a < 0.5 && transparent < 0.5) {
        discard;
    }
    // Chunks demodulate by albedo; water/glass denoise in radiance space (ca = 1).
    let ca = select(vec3<f32>(1.0), max(albedo4.rgb, vec3<f32>(0.04)), albedo4.a >= 0.5);
    let ci = center_color / ca;
    let cl = dot(ci, vec3<f32>(0.299, 0.587, 0.114));
    let cp = world_at(c, dims, cd);

    // ── Spatial: à-trous bilateral on the current frame's irradiance, and a tight 3×3
    // min/max box of the raw irradiance for the temporal clamp. ──
    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    var bmin = vec3<f32>(1e9);
    var bmax = vec3<f32>(-1e9);
    let step = 2;
    for (var dy = -3; dy <= 3; dy = dy + 1) {
        for (var dx = -3; dx <= 3; dx = dx + 1) {
            let p = clamp(c + vec2<i32>(dx, dy) * step, vec2<i32>(0), dims - vec2<i32>(1));
            let d = textureLoad(depth_tex, p, 0).r;
            if (d >= 1.0) {
                continue; // skip sky neighbours
            }
            let wp = world_at(p, dims, d);
            let diff = wp - cp;
            let dw = exp(-dot(diff, diff) * 4.0);
            let r2 = f32(dx * dx + dy * dy);
            let sw = exp(-r2 * 0.15);
            let ip = irradiance_at(p);
            let il = dot(ip, vec3<f32>(0.299, 0.587, 0.114));
            let lw = exp(-abs(il - cl) / (cl * 0.6 + 0.1));
            let w = dw * sw * lw;
            sum = sum + ip * w;
            wsum = wsum + w;
            // 3×3 (step-1) raw box for the history clamp.
            if (abs(dx) <= 1 && abs(dy) <= 1) {
                let q = clamp(c + vec2<i32>(dx, dy), vec2<i32>(0), dims - vec2<i32>(1));
                let qi = irradiance_at(q);
                bmin = min(bmin, qi);
                bmax = max(bmax, qi);
            }
        }
    }
    // Water/glass reflections are sharp — skip the spatial blur (it softens them) and let
    // the temporal accumulation below do the cleaning. Chunks get the full spatial filter.
    let spatial = select(ci, sum / wsum, wsum > 0.0);
    let cur = select(spatial, ci, albedo4.a < 0.5);

    // ── Temporal: reproject + neighbourhood-clamp + accumulate. ──
    var acc = cur;
    let mv = textureLoad(mv_tex, c, 0).rg;
    let prev_uv = in.uv - mv;
    if (all(prev_uv >= vec2<f32>(0.0)) && all(prev_uv <= vec2<f32>(1.0))) {
        let hist = textureSampleLevel(hist_tex, hist_samp, prev_uv, 0.0).rgb;
        // Clamp the (smooth) history into the current raw neighbourhood — rejects ghosting
        // and disocclusion (the box shifts when a new surface is reprojected in).
        let clamped = clamp(hist, bmin, bmax);
        acc = mix(cur, clamped, 0.85); // ~7-frame accumulation
    }

    o.hist = vec4<f32>(acc, 1.0);
    o.view = vec4<f32>(acc * ca, 1.0); // remodulate with THIS pixel's sharp albedo
    return o;
}
