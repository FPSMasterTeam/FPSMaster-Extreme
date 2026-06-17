// Entity-armor enchantment glint: the same purple shimmer as `glint.wgsl`, but
// driven by the model-pass vertex layout (`ModelVertex`: position/color/uv, no
// per-vertex light) and masked against the entity atlas. The renderer re-draws
// the enchanted-armor geometry additively with `enchanted_item_glint.png`,
// scrolled diagonally over time in two opposing layers like vanilla
// `renderEffect`.

struct Camera {
    view_proj: mat4x4<f32>,
    sky_brightness: f32,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

// The entity atlas (sampled only for the alpha cutout, so the glint is masked to
// the armor silhouette) and the repeating glint texture at group 3.
@group(1) @binding(0)
var entity_atlas: texture_2d<f32>;
@group(1) @binding(1)
var entity_sampler: sampler;

struct Glint {
    scroll: f32,
};
@group(2) @binding(0)
var<uniform> glint: Glint;

@group(3) @binding(0)
var glint_tex: texture_2d<f32>;
@group(3) @binding(1)
var glint_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(input.position, 1.0);
    out.uv = input.uv;
    return out;
}

const GLINT_COLOR = vec3<f32>(0.38, 0.19, 0.608);

fn glint_uv(uv: vec2<f32>, scroll: f32, angle: f32) -> vec2<f32> {
    let c = cos(angle);
    let s = sin(angle);
    let r = vec2<f32>(uv.x * c - uv.y * s, uv.x * s + uv.y * c);
    return r * 8.0 + vec2<f32>(scroll, 0.0);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Mask to the armor: skip glint where the atlas texel is transparent so the
    // shimmer hugs the silhouette (same cutout the model draw uses).
    let base = textureSample(entity_atlas, entity_sampler, input.uv);
    if (base.a < 0.5) {
        discard;
    }
    let a = textureSample(glint_tex, glint_sampler, glint_uv(input.uv, glint.scroll, -0.8727)).rgb;
    let b = textureSample(glint_tex, glint_sampler, glint_uv(input.uv, -glint.scroll * 2.0, 0.8727)).rgb;
    let sheen = (a + b) * GLINT_COLOR;
    return vec4<f32>(sheen, 1.0);
}
