// Enchantment glint: the purple shimmer vanilla draws over an enchanted item or
// armor by re-rendering the geometry additively with `enchanted_item_glint.png`
// (`RenderItem.renderEffect`). The glint texture is sampled at the item's own
// surface UV, scaled up and scrolled diagonally over time; vanilla draws two
// layers scrolling in opposite directions for the moving sheen. Additive blend
// is configured on the pipeline (src·alpha + dst), so the dark glint texels add
// nothing while the bright streaks light the item.

struct Camera {
    view_proj: mat4x4<f32>,
    sky_brightness: f32,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

// The item's normal atlas texture (sampled only for the alpha cutout, so the
// glint is masked to the item silhouette) and the repeating glint texture.
@group(1) @binding(0)
var item_atlas: texture_2d<f32>;
@group(1) @binding(1)
var item_sampler: sampler;

struct Glint {
    // Scroll offset in glint-texture space, advanced each frame from a clock.
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
    @location(3) light: vec2<f32>,
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

// The vanilla glint tint (0.38, 0.19, 0.608) — a deep violet.
const GLINT_COLOR = vec3<f32>(0.38, 0.19, 0.608);

// Rotate the surface UV into the glint frame (vanilla rotates the glint texture
// matrix by -50°), then scale it up so the streaks repeat across the sprite.
fn glint_uv(uv: vec2<f32>, scroll: f32, angle: f32) -> vec2<f32> {
    let c = cos(angle);
    let s = sin(angle);
    let r = vec2<f32>(uv.x * c - uv.y * s, uv.x * s + uv.y * c);
    return r * 8.0 + vec2<f32>(scroll, 0.0);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Mask to the item: skip glint where the item itself is transparent so the
    // shimmer hugs the silhouette (same cutout the item draw uses).
    let base = textureSample(item_atlas, item_sampler, input.uv);
    if (base.a < 0.5) {
        discard;
    }
    // Two opposing layers like vanilla, summed for the travelling sheen.
    let a = textureSample(glint_tex, glint_sampler, glint_uv(input.uv, glint.scroll, -0.8727)).rgb;
    let b = textureSample(glint_tex, glint_sampler, glint_uv(input.uv, -glint.scroll * 2.0, 0.8727)).rgb;
    let sheen = (a + b) * GLINT_COLOR;
    return vec4<f32>(sheen, 1.0);
}
