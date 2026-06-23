// Per-entity motion-vector pass: re-renders the entity model geometry (mobs,
// players, chests, signs, first-person arm) into the RG16F motion-vector buffer,
// depth-tested against the world depth so only visible entity pixels overwrite
// the camera-only vectors there. Captures each entity's own world movement
// (rigid root translation, carried per-vertex in `motion.xyz`) on top of camera
// motion, so walking mobs no longer ghost under TAA.
//
// Depth must match the main pass (which rendered with the JITTERED projection),
// so `@builtin(position)` is rebuilt jittered. The velocity itself is computed
// from the UN-jittered current/previous matrices, keeping the motion vectors
// jitter-free (the jitter is handed to the temporal resolve separately).

struct Vel {
    cur_view_proj: mat4x4<f32>,   // current camera-relative VP, un-jittered
    prev_view_proj: mat4x4<f32>,  // previous-frame camera-relative VP, un-jittered
    jitter: vec4<f32>,            // xy = this frame's NDC jitter (applied for depth)
    cur_origin: vec4<i32>,        // current render origin (whole blocks)
    prev_origin: vec4<i32>,       // previous-frame render origin
};

@group(0) @binding(0) var<uniform> vel: Vel;

struct VertexInput {
    @location(0) position: vec3<f32>,  // absolute world position (ModelVertex)
    // location 1 (color) and 2 (uv) are present in the buffer but unused here.
    @location(3) motion: vec4<f32>,    // xyz = world delta this frame, w = hand flag
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) cur_clip: vec4<f32>,
    @location(1) prev_clip: vec4<f32>,
    @location(2) hand: f32,
};

@vertex
fn vs_main(in: VertexInput) -> VsOut {
    var out: VsOut;
    let rel_cur = in.position - vec3<f32>(vel.cur_origin.xyz);
    let cur_clip = vel.cur_view_proj * vec4<f32>(rel_cur, 1.0);
    // Previous-frame world position = current minus this frame's rigid movement.
    let prev_world = in.position - in.motion.xyz;
    let rel_prev = prev_world - vec3<f32>(vel.prev_origin.xyz);
    let prev_clip = vel.prev_view_proj * vec4<f32>(rel_prev, 1.0);

    // Rasterize at the jittered current position so depth matches the main pass.
    var jittered = cur_clip;
    jittered.x += vel.jitter.x * cur_clip.w;
    jittered.y += vel.jitter.y * cur_clip.w;

    out.pos = jittered;
    out.cur_clip = cur_clip;
    out.prev_clip = prev_clip;
    out.hand = in.motion.w;
    return out;
}

fn clip_to_uv(clip: vec4<f32>) -> vec2<f32> {
    let ndc = clip.xy / clip.w;
    return vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec2<f32> {
    // Screen-locked geometry (the first-person arm): zero velocity, so TAA holds
    // it in place instead of reprojecting it by camera motion.
    if (in.hand > 0.5) {
        return vec2<f32>(0.0, 0.0);
    }
    // mv = cur_uv - prev_uv (subtract from a pixel's uv to fetch its prev-frame
    // location) — same convention as the camera-only motion-vector pass.
    return clip_to_uv(in.cur_clip) - clip_to_uv(in.prev_clip);
}
