// Temporal anti-aliasing resolve. Blends the current (jittered) HDR scene with
// the previous resolved frame, reprojected through the motion-vector buffer, and
// clamps the history to the current 3x3 neighbourhood colour box to suppress
// ghosting on motion / disocclusion. The output feeds bloom / auto-exposure /
// post and is copied into the history texture for the next frame.
//
// Together with the sub-pixel camera jitter (each frame samples a slightly
// different point inside the pixel) this accumulates an anti-aliased image over
// several frames. Camera-only motion vectors mean independently-moving entities
// are reprojected by camera motion only; the neighbourhood clamp hides the
// residual.
//
// Sharpness: the history is resampled with a Catmull-Rom (bicubic) filter rather
// than plain bilinear — bilinear history resampling is the main cause of TAA
// "motion blur", because every reprojected frame softens a bit. The history
// weight is also lowered as on-screen motion grows, so fast camera movement
// leans on the (sharp) current frame instead of the accumulated history.

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var history_tex: texture_2d<f32>;
@group(0) @binding(2) var mv_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

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

// 9-tap Catmull-Rom history sample (Pettineo's bilinear-tap form): covers the
// 4x4 bicubic footprint with 9 linear fetches. Sharper than bilinear, so the
// history stops softening every frame. Can overshoot slightly (bicubic
// ringing); the neighbourhood clamp below pulls it back, and we keep it >= 0.
fn sample_history(uv: vec2<f32>, dims: vec2<f32>) -> vec3<f32> {
    let tex_pos = uv * dims;
    let center = floor(tex_pos - 0.5) + 0.5;
    let f = tex_pos - center;

    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let offset12 = w2 / w12;

    let tc0 = (center - 1.0) / dims;
    let tc3 = (center + 2.0) / dims;
    let tc12 = (center + offset12) / dims;

    var r = vec3<f32>(0.0);
    r += textureSampleLevel(history_tex, samp, vec2<f32>(tc0.x, tc0.y), 0.0).rgb * (w0.x * w0.y);
    r += textureSampleLevel(history_tex, samp, vec2<f32>(tc12.x, tc0.y), 0.0).rgb * (w12.x * w0.y);
    r += textureSampleLevel(history_tex, samp, vec2<f32>(tc3.x, tc0.y), 0.0).rgb * (w3.x * w0.y);

    r += textureSampleLevel(history_tex, samp, vec2<f32>(tc0.x, tc12.y), 0.0).rgb * (w0.x * w12.y);
    r += textureSampleLevel(history_tex, samp, vec2<f32>(tc12.x, tc12.y), 0.0).rgb * (w12.x * w12.y);
    r += textureSampleLevel(history_tex, samp, vec2<f32>(tc3.x, tc12.y), 0.0).rgb * (w3.x * w12.y);

    r += textureSampleLevel(history_tex, samp, vec2<f32>(tc0.x, tc3.y), 0.0).rgb * (w0.x * w3.y);
    r += textureSampleLevel(history_tex, samp, vec2<f32>(tc12.x, tc3.y), 0.0).rgb * (w12.x * w3.y);
    r += textureSampleLevel(history_tex, samp, vec2<f32>(tc3.x, tc3.y), 0.0).rgb * (w3.x * w3.y);

    return max(r, vec3<f32>(0.0));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(scene_tex));
    let texel = 1.0 / dims;
    let cur = textureSampleLevel(scene_tex, samp, in.uv, 0.0).rgb;

    // Variance clipping of the 3x3 neighbourhood (Salvi): clip the reprojected
    // history to mean +/- gamma*stddev instead of the min/max box. On
    // high-frequency content (grass, leaves) the min/max box is so wide that a
    // wrong reprojected sample fits inside it and accumulates into a smear —
    // worst on forward/zoom motion. The statistical box is far tighter, so stale
    // history is rejected and the streaking disappears (at the cost of a little
    // more aliasing while moving, which is the right trade).
    var m1 = vec3<f32>(0.0);
    var m2 = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let c = textureSampleLevel(
                scene_tex, samp, in.uv + vec2<f32>(f32(x), f32(y)) * texel, 0.0).rgb;
            m1 += c;
            m2 += c * c;
        }
    }
    let inv_n = 1.0 / 9.0;
    let mean = m1 * inv_n;
    let sigma = sqrt(max(m2 * inv_n - mean * mean, vec3<f32>(0.0)));
    // Tight statistical box (mean +/- 0.6 sigma): rejects mis-reprojected history
    // (the forward-motion grass smear) while still allowing in-distribution
    // history to accumulate for anti-aliasing. This — not a low history weight —
    // is what kills the smear, so the weight can stay high enough to average out
    // the jitter (otherwise rotation shows the raw jittered frame and flickers).
    let gamma = 0.6;
    let cmin = mean - gamma * sigma;
    let cmax = mean + gamma * sigma;

    // Reproject into the previous frame: mv = cur_uv - prev_uv, so prev_uv = uv - mv.
    let px = vec2<i32>(clamp(in.uv * dims, vec2<f32>(0.0), dims - 1.0));
    let mv = textureLoad(mv_tex, px, 0).rg;
    let hist_uv = in.uv - mv;

    // History off-screen (camera turned to reveal a new edge): no accumulation.
    if (hist_uv.x < 0.0 || hist_uv.x > 1.0 || hist_uv.y < 0.0 || hist_uv.y > 1.0) {
        return vec4<f32>(cur, 1.0);
    }

    var hist = sample_history(hist_uv, dims);
    hist = clamp(hist, cmin, cmax);

    // Keep the history weight high so the sub-pixel jitter is averaged out (a low
    // weight shows the raw jittered frame, which flickers under rotation). The
    // tight neighbourhood clamp above does the ghost/smear rejection. Only a mild
    // drop at high on-screen speed, for disocclusion safety.
    let speed_px = length(mv * dims);
    let hist_weight = mix(0.9, 0.8, clamp(speed_px / 40.0, 0.0, 1.0));
    return vec4<f32>(mix(cur, hist, hist_weight), 1.0);
}
