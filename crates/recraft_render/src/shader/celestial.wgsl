// Sun, moon and stars: textured quads authored at infinity in the celestial
// frame (already rotated on the CPU) and projected with the rotation-only
// view-projection, so they wheel with the day but never translate with the
// player. Shares the chunk `Vertex` layout; the `light` attribute is unused
// here (the sky is not affected by the world lightmap).

struct Celestial {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> celestial: Celestial;

@group(1) @binding(0)
var sky_atlas: texture_2d<f32>;
@group(1) @binding(1)
var sky_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) light: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = celestial.view_proj * vec4<f32>(input.position, 1.0);
    // Pin to the far plane (NDC z = 1.0) so the depth test (LessEqual vs the
    // cleared far depth) lets opaque terrain occlude the sun/moon/stars while
    // they still fill the open sky. Drawn after terrain to skip the fill behind
    // blocks. x/y/w are untouched, so the on-screen position is unchanged.
    out.clip_position.z = out.clip_position.w;
    out.color = input.color;
    out.uv = input.uv;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(sky_atlas, sky_sampler, input.uv);
    let c = texel.rgb * input.color.rgb;
    // HDR boost for the bright sun disc so it overflows into a radiant bloom (the
    // "epic"/divine sun). Gated on luminance, so the much dimmer moon and stars are
    // barely touched.
    let lum = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let hdr = c * (1.0 + smoothstep(0.45, 0.85, lum) * 7.0);
    return vec4<f32>(hdr, texel.a * input.color.a);
}
