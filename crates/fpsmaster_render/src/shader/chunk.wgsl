struct Camera {
    view_proj: mat4x4<f32>,
    // Day/night sky-light scale (vanilla getSunBrightness): 1.0 by day, ~0.2 at
    // night. Applied to each vertex's sky-light term so open ground darkens at
    // night while block-lit (torch/lava) surfaces keep their brightness.
    sky_brightness: f32,
    time: f32,
    tile_size: vec2<f32>,
    // Render origin in whole blocks: subtracted from every world position so the
    // view-projection (built with the camera at this origin) stays precise far
    // from spawn. xyz used; w padding.
    origin: vec4<i32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(1) @binding(0)
var block_atlas: texture_2d<f32>;

@group(1) @binding(1)
var block_sampler: sampler;

// Shader-pack lighting (group 2): directional sun + ambient (+ shadows later).
struct Lighting {
    light_view_proj: mat4x4<f32>,
    sun_dir: vec4<f32>,    // xyz: world-space direction TO the sun (normalized)
    sun_color: vec4<f32>,  // rgb sun intensity
    ambient: vec4<f32>,    // rgb ambient intensity
    camera_pos: vec4<f32>, // xyz: world-space eye (for specular)
    // x = master enable, y = shadows, z = specular, w = shadow map texel size.
    flags: vec4<f32>,
    fog_color: vec4<f32>,  // rgb: fog (horizon) colour
    fog_params: vec4<f32>, // x: start dist, y: end dist, z: enabled, w: brightness
    extra: vec4<f32>,      // x: fullbright (1 = force lightmap to full)
};

@group(2) @binding(0)
var<uniform> lighting: Lighting;
@group(2) @binding(1)
var shadow_map: texture_depth_2d;
@group(2) @binding(2)
var shadow_sampler: sampler_comparison;
@group(2) @binding(3)
var normal_atlas: texture_2d<f32>;
@group(2) @binding(4)
var specular_atlas: texture_2d<f32>;
@group(2) @binding(5)
var pbr_sampler: sampler;

struct VertexInput {
    // xyz: world position × 64 as fixed-point i16.
    // w: packed (sky_u8 << 8 | block_u8) reinterpreted as i16.
    @location(0) pos_light: vec4<i32>,
    // RGBA8 unorm: tint × face shade × AO + alpha.
    @location(1) color: vec4<f32>,
    // Normalized atlas UV as Unorm16×2.
    @location(2) uv: vec2<f32>,
    // Geometric face normal (Snorm8×4, xyz used).
    @location(3) normal: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) light: vec2<f32>,
    @location(3) world_pos: vec3<f32>,
    @location(4) normal: vec3<f32>,
    // 1.0 for surfaces of a self-emissive block (lava/glowstone/fire/…), packed in
    // bit 16 of pos_light.w by the mesher — the only reliable emitter marker (the
    // voxel block-light is also high on lit NEIGHBOURS, which caused glow rings).
    @location(5) emissive: f32,
    // 1.0 where the biome gets snow rather than rain (bit 17).
    @location(6) snowy: f32,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // Camera-relative: subtract the render origin in fixed-point (×64) i32 before
    // converting to f32, so the position stays exact at any world coordinate.
    let rel = input.pos_light.xyz - camera.origin.xyz * 64;
    let world_pos = vec3<f32>(f32(rel.x), f32(rel.y), f32(rel.z)) / 64.0;
    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    let w_bits = u32(input.pos_light.w) & 0xFFFFu;
    out.light = vec2<f32>(f32((w_bits >> 8u) & 0xFFu) / 255.0, f32(w_bits & 0xFFu) / 255.0);
    out.emissive = f32((u32(input.pos_light.w) >> 16u) & 1u);
    out.snowy = f32((u32(input.pos_light.w) >> 17u) & 1u);
    out.world_pos = world_pos;
    out.normal = input.normal.xyz;
    return out;
}

fn day_night(light: vec2<f32>) -> f32 {
    return max(light_level(light.x) * camera.sky_brightness, light_level(light.y));
}

// sRGB -> linear (the same curve `ui.wgsl` applies to its vertex colours).
//
// Vanilla 1.8.9 uploads its terrain atlas as plain GL_RGBA and never enables
// GL_FRAMEBUFFER_SRGB, so every shading multiplier it applies — the 1.0/0.8/0.6/0.5
// face shades, the 1.0/0.8/0.6/0.4 AO ladder, the biome tint, the lightmap — lands
// on the GAMMA-ENCODED texel. Our atlas is Rgba8UnormSrgb, so `textureSample`
// returns LINEAR values and the swapchain re-encodes on write. Multiplying a
// linear texel by those same constants therefore raises each one to ~1/2.2 on
// screen (a 0.6 side-face shade would show as 0.80, the darkest AO corner 0.4 as
// 0.67), collapsing the contrast ladder into the flat, washed-out look.
//
// So: keep combining the multipliers in vanilla's gamma space, then decode the
// product once here before it touches the linear texel.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

// The vertex colour (biome tint × face shade × AO) is authored by the mesher in
// vanilla's gamma space, so decode it before it modulates the linear texel.
fn albedo_of(texel: vec3<f32>, vertex_color: vec3<f32>) -> vec3<f32> {
    return texel * srgb_to_linear(vertex_color);
}

// Vanilla per-level brightness curve: light falls off steeply toward the dark
// end (l=1 -> 1, l=0.5 -> 0.2), giving the moody gradient around light sources.
fn light_level(l: f32) -> f32 {
    return l / (4.0 - 3.0 * clamp(l, 0.0, 1.0));
}

// Vanilla's torch flicker: `updateTorchFlicker` runs a damped random walk around
// 0 and feeds it in as `torchFlickerX * 0.1 + 1.5`. A random walk can't be
// reproduced per-fragment, so this is a cheap smooth stand-in with the same tiny
// amplitude — the point is only that torch-lit surfaces breathe.
fn torch_gain() -> f32 {
    let t = camera.time;
    let flicker = sin(t * 3.1) * 0.6 + sin(t * 7.7) * 0.3 + sin(t * 13.3) * 0.15;
    return flicker * 0.1 + 1.5;
}

// Vanilla's coloured light map, ported from `EntityRenderer.updateLightmap`.
//
// The sky and block terms are read through the vanilla brightness table, tinted
// separately, then SUMMED. This used to be a `max()`, which meant a torch could
// never brighten a surface the sky already lit — indoor/outdoor transitions came
// out flat and daylight swallowed torchlight entirely. The `* 1.5` block gain
// was also missing, leaving lit interiors about a third too dim.
fn vanilla_lightmap(light: vec2<f32>) -> vec3<f32> {
    let sun = camera.sky_brightness;
    let sky = light_level(light.x) * (sun * 0.95 + 0.05);
    let block = light_level(light.y) * torch_gain();
    // Sky: red and green are damped by the day factor while blue is left alone,
    // which is what makes vanilla's night read cold blue rather than plain grey.
    let sky_rg = sky * (sun * 0.65 + 0.35);
    // Block: red passes through, green and blue are progressively damped, so a
    // torch is orange where it is dim and washes toward white where it is bright
    // (the old fixed `(1.0, 0.60, 0.30)` made every block-lit surface equally
    // orange no matter the level).
    let block_g = block * ((block * 0.6 + 0.4) * 0.6 + 0.4);
    let block_b = block * (block * block * 0.6 + 0.4);
    let rgb = vec3<f32>(sky_rg + block, sky_rg + block_g, sky + block_b);
    return clamp(rgb * 0.96 + vec3<f32>(0.03), vec3<f32>(0.0), vec3<f32>(1.0));
}

/// Absolute world position from the camera-relative one the vertex stage emits.
///
/// `VertexOutput.world_pos` is RELATIVE to the render origin (the camera's block
/// position), which is what keeps f32 precise far from spawn. Anything anchored
/// to the world — noise fields, plane intersections — must add the origin back,
/// or it drifts with the player: the puddle field slid across the ground every
/// time the origin stepped.
fn absolute_pos(world_pos: vec3<f32>) -> vec3<f32> {
    return world_pos + vec3<f32>(camera.origin.xyz);
}

// --- Cloud shadows ------------------------------------------------------
//
// Dappled light moving across the ground is most of what makes an outdoor scene
// feel alive rather than a static diorama.
//
// This uses its own cheap value noise rather than sampling the sky pass's 3D
// cloud texture, which is bound to a different pipeline: reaching it from here
// would mean widening the lit bind-group layout that every world pipeline
// shares. The shadow field therefore does NOT correspond texel-for-texel to the
// clouds actually drawn overhead — but it runs at the same world scale and wind
// speed, and with the deck at y=220 a cloud's shadow lands far from the cloud
// anyway, so the mismatch is not perceivable from the ground. Most shader packs
// make the same trade.
fn cloud_hash(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

fn cloud_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(cloud_hash(i), cloud_hash(i + vec2<f32>(1.0, 0.0)), u.x),
        mix(cloud_hash(i + vec2<f32>(0.0, 1.0)), cloud_hash(i + vec2<f32>(1.0, 1.0)), u.x),
        u.y,
    );
}

/// Fraction of sunlight reaching `world_pos` through the cloud deck.
fn cloud_shadow(rel_pos: vec3<f32>, overcast: f32) -> f32 {
    let world_pos = absolute_pos(rel_pos);
    let dir = lighting.sun_dir.xyz;
    // A sun near the horizon casts a near-infinite ray through the deck; there is
    // no meaningful cloud shadow left to compute.
    if (dir.y < 0.08) {
        return 1.0;
    }
    // Same plane height, wind and world scale as the drawn clouds (sky.wgsl).
    let t = (220.0 - world_pos.y) / dir.y;
    let wind = vec2<f32>(camera.time * 1.5, camera.time * 0.5);
    let uv = (world_pos.xz + dir.xz * t + wind) * 0.0016;
    let n = cloud_noise(uv) * 0.65 + cloud_noise(uv * 2.7) * 0.35;
    // Matches the sky pass's coverage ramp so the deck thickens with the weather.
    let coverage = 0.5 + 0.25 * overcast;
    let d = smoothstep(1.0 - coverage - 0.18, 1.0 - coverage + 0.12, n);
    // Clouds scatter a lot of light through, so a shadow never goes fully dark.
    return 1.0 - d * 0.55;
}

// --- Rain wetness, puddles and ripples ----------------------------------
//
// Naive "wetness" (darken everything by a constant) reads as a grey wash. The
// techniques below are what shader packs have converged on, and each is
// standard real-time practice rather than anything Minecraft-specific:
//
//  * POROSITY drives the response. A porous surface (dirt, wool) soaks water
//    up: it darkens a lot and stays rough. A dense one (stone, glass) barely
//    darkens but turns glossy. One flat darkening for every block is the single
//    biggest reason a wet effect looks fake.
//  * PUDDLES are a low-frequency world-space noise field thresholded against
//    the wetness level, so they spread as the storm sets in — gated to surfaces
//    the sky actually reaches, and to up-facing faces.
//  * Water's Fresnel F0 is ~0.02: a puddle is nearly invisible head-on and
//    almost a mirror at grazing angles. That ramp IS the look; a constant
//    reflection strength gets it wrong at both ends.
//  * RIPPLES perturb the puddle's normal with expanding rings on a jittered
//    grid, which is what makes rain read as actively falling rather than as a
//    static gloss coat someone painted on.

/// Where standing water has collected, 0..1.
fn puddle_mask(rel_pos: vec3<f32>, sky: f32, up: f32, wetness: f32) -> f32 {
    if (wetness < 0.001) {
        return 0.0;
    }
    let world_pos = absolute_pos(rel_pos);
    // Scale matters more than anything else here. `cloud_noise` is an infinite
    // lattice value noise, so one unit of input is one cell; a tiling noise
    // TEXTURE (what shader packs sample) repeats every 64-256 texels and carries
    // far higher frequency at the same multiplier. Copying their multipliers
    // gave 40- and 160-block cells, which put the whole visible scene inside a
    // single cell — the mask came out constant and the ground just uniformly
    // darkened, with no puddle shapes at all.
    //
    // Pools want to be a handful of blocks across, with a finer octave breaking
    // up the edges.
    let p = world_pos.xz;
    // Three octaves: the finest one breaks up the outline so a pool has a ragged
    // edge instead of a clean contour.
    var n = cloud_noise(p * 0.9) * 0.15
        + cloud_noise(p * 0.33) * 0.3
        + cloud_noise(p * 0.08) * 0.55;
    // Driven by accumulated standing water, NOT the rain strength: pools should
    // fill over tens of seconds, not snap in with the weather change.
    n = n + wetness * 0.42 - 0.34;
    // A wide threshold window: a narrow one cut a hard shoreline between wet and
    // dry, which is the one thing a puddle never has.
    var puddles = smoothstep(0.20, 0.80, n);
    // Near-max skylight only: ground under a tree or against a wall stays dry.
    puddles = puddles * clamp(sky * 8.0 - 7.0, 0.0, 1.0) * clamp(up, 0.0, 1.0);
    return puddles;
}

/// One ripple field sample — expanding rings on a jittered grid, each cell
/// firing on its own phase so they do not pulse in unison.
fn ripple_height(rel_pos: vec3<f32>, offset: vec2<f32>) -> f32 {
    let p = absolute_pos(rel_pos).xz + offset;
    let cell = floor(p);
    let f = fract(p);
    let seed = cloud_hash(cell);
    let t = camera.time * 1.7 + seed;
    let phase = fract(t);
    let centre = vec2<f32>(cloud_hash(cell + 1.7), cloud_hash(cell + 3.1)) * 0.5 + 0.25;
    var r = clamp(1.0 - 4.0 * length(f - centre), 0.0, 1.0);
    // Ring expands over the cell's phase, then fades.
    r = clamp(r + phase - 1.0, 0.0, 1.0);
    return sin(min(r * 6.0 * 3.14159, 3.0 * 3.14159)) * pow(1.0 - phase, 2.0);
}

/// Slope of the ripple field, as an xz normal perturbation (4-tap finite
/// difference — the standard way to get a normal out of a height function).
fn ripple_slope(world_pos: vec3<f32>) -> vec2<f32> {
    let e = 0.15;
    let h1 = ripple_height(world_pos, vec2<f32>(e, 0.0));
    let h2 = ripple_height(world_pos, vec2<f32>(-e, 0.0));
    let h3 = ripple_height(world_pos, vec2<f32>(0.0, e));
    let h4 = ripple_height(world_pos, vec2<f32>(0.0, -e));
    return vec2<f32>((h2 - h1) / e, (h4 - h3) / e) * 0.35;
}

// Fraction of the sun visible at this world position (1 = lit, 0 = shadowed),
// 3×3 PCF. Outside the shadow map's range everything is lit.
fn sun_shadow(world_pos: vec3<f32>, ndotl: f32) -> f32 {
    let lc = lighting.light_view_proj * vec4<f32>(world_pos, 1.0);
    let ndc = lc.xyz / lc.w;
    if (ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z > 1.0) {
        return 1.0;
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
    let bias = clamp(0.0015 * (1.0 - ndotl), 0.0004, 0.003);
    let depth = ndc.z - bias;
    let texel = lighting.flags.w;
    var sum = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * texel;
            sum = sum + textureSampleCompare(shadow_map, shadow_sampler, uv + off, depth);
        }
    }
    return sum / 9.0;
}

// Vanilla brightness ("gamma") curve (EntityRenderer.updateLightmap): the
// setting in 0..1 blends from the base curve (0, Moody) toward a lifted curve
// `1 - (1-x)^4` (1, Bright) — it only ever BRIGHTENS the dark end, never
// darkens below base. Values above 1 (bright sun on the shader path) pass
// through so daylight keeps its punch.
fn light_curve(light: vec3<f32>, brightness: f32) -> vec3<f32> {
    let low = clamp(light, vec3<f32>(0.0), vec3<f32>(1.0));
    let inv = vec3<f32>(1.0) - low;
    let lifted = vec3<f32>(1.0) - inv * inv * inv * inv;
    let high = max(light - vec3<f32>(1.0), vec3<f32>(0.0));
    return mix(low, lifted, brightness) + high;
}

// Vanilla applies the brightness gamma to the clamped lightmap and then repeats
// the `* 0.96 + 0.03` lift before writing the lightmap texture. `vanilla_lightmap`
// already clamps to 0..1, so `light_curve`'s HDR passthrough stays inert here.
fn vanilla_lightmap_graded(light: vec2<f32>, gamma: f32) -> vec3<f32> {
    let lit = light_curve(vanilla_lightmap(light), gamma);
    return clamp(lit * 0.96 + vec3<f32>(0.03), vec3<f32>(0.0), vec3<f32>(1.0));
}

// GGX normal distribution function.
fn ggx_distribution(ndoth: f32, roughness: f32) -> f32 {
    let a2 = roughness * roughness;
    let d = ndoth * ndoth * (a2 - 1.0) + 1.0;
    return a2 / (3.14159 * d * d);
}

// Smith geometry term (GGX, combined masking-shadowing).
fn smith_ggx(ndotv: f32, ndotl: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    let gv = ndotv / (ndotv * (1.0 - k) + k);
    let gl = ndotl / (ndotl * (1.0 - k) + k);
    return gv * gl;
}

// Schlick Fresnel approximation.
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - cos_theta, 5.0);
}

// Build TBN from geometric face normal (axis-aligned in Minecraft) and sample
// the tangent-space normal from the PBR normal atlas.
fn sample_pbr_normal(uv: vec2<f32>, geo_n: vec3<f32>) -> vec3<f32> {
    let n_tex = textureSample(normal_atlas, pbr_sampler, uv);
    var ts = n_tex.rgb * 2.0 - vec3<f32>(1.0);
    ts.x = ts.x * 1.5;
    ts.y = ts.y * 1.5;
    let abs_n = abs(geo_n);
    var tangent: vec3<f32>;
    var bitangent: vec3<f32>;
    if (abs_n.y > 0.9) {
        tangent = vec3<f32>(1.0, 0.0, 0.0);
        bitangent = vec3<f32>(0.0, 0.0, sign(geo_n.y));
    } else if (abs_n.x > 0.9) {
        tangent = vec3<f32>(0.0, 0.0, -sign(geo_n.x));
        bitangent = vec3<f32>(0.0, 1.0, 0.0);
    } else {
        tangent = vec3<f32>(sign(geo_n.z), 0.0, 0.0);
        bitangent = vec3<f32>(0.0, 1.0, 0.0);
    }
    return normalize(tangent * ts.x + bitangent * ts.y + geo_n * ts.z);
}

// Apply the shader-pack lighting model to an albedo colour. Falls back to the
// vanilla flat day/night brightness when shaders are disabled.
fn apply_lighting(albedo: vec3<f32>, in: VertexOutput) -> vec3<f32> {
    // Fullbright preset: skip all lightmap/shader darkening (keep baked shading).
    if (lighting.extra.x > 0.5) {
        return albedo;
    }
    // Brightness option (fog_params.w in 0..1), fed straight to the vanilla
    // gamma blend: 0 = Moody base curve, 1 = fully lifted (Bright).
    let gamma = lighting.fog_params.w;
    if (lighting.flags.x < 0.5) {
        // Vanilla path: the coloured light map, then the brightness gamma. The
        // lightmap is a vanilla gamma-space multiplier like the vertex colour, so
        // it is decoded before it scales the linear albedo (see `srgb_to_linear`).
        return albedo * srgb_to_linear(vanilla_lightmap_graded(in.light, gamma));
    }
    let geo_n = normalize(in.normal);
    let pbr_on = lighting.fog_color.w > 0.5;
    let n = select(geo_n, sample_pbr_normal(in.uv, geo_n), pbr_on);
    let ndotl = max(dot(n, lighting.sun_dir.xyz), 0.0);
    // Sun visibility (1 = lit, 0 = shadowed): rasterized shadow-map PCF by default,
    // or hardware ray-traced (sharp/soft) when ray tracing is active. The two
    // variants are supplied by the prepended rt_stub.wgsl / rt_common.wgsl.
    // `in.clip_position.xy` is the framebuffer pixel coord, used to seed the RT noise.
    // Overcast dissolves the sun's shadow: with the sky as one big area light
    // there is no sharp shadow to cast, and leaving the shadow map at full
    // strength during rain drew knife-edged shadows under a sunless sky.
    let overcast = lighting.extra.y;
    let shadow = mix(
        sun_visibility(in.world_pos, geo_n, ndotl, in.clip_position.xy),
        1.0,
        overcast,
    );
    let sky = in.light.x;
    let block = in.light.y;
    let day = max(camera.sky_brightness, 0.04);
    // Ambient term. The sky-dependent part is scaled down when the ray-traced sky GI
    // is active (it adds the directional sky lighting additively afterward); a small
    // base is always kept. With RT off, gi_ambient_scale() = 1 (unchanged).
    //
    // Sky light is a HEMISPHERE, not a uniform glow: it arrives from above, so an
    // up-facing surface sees the whole sky, a wall sees half of it, and an
    // overhang sees only bounce off the ground. This used to be one flat term
    // applied to every face, which was survivable while the sun supplied the
    // directional cue — but once overcast removes the sun there is nothing left,
    // and the whole scene flattens into a single tone.
    let hemi = mix(0.6, 1.2, geo_n.y * 0.5 + 0.5);
    // Tint the up-facing half toward the sky's own colour. Luma-normalised, so
    // this shifts hue only and cannot change the overall exposure — ambient then
    // tracks the time of day and the weather instead of being a fixed grey-blue.
    let sky_luma = max(dot(lighting.fog_color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722)), 1e-4);
    let sky_hue = lighting.fog_color.rgb / sky_luma;
    let ambient_tint = mix(vec3<f32>(1.0), sky_hue, 0.35 * max(geo_n.y, 0.0));
    let ambient = lighting.ambient.rgb
        * hemi
        * ambient_tint
        * (0.08 + 0.92 * sky * day * gi_ambient_scale());
    // Block (torch/lava) light: the smooth voxel block-light, scaled down when RT point
    // lights are active, PLUS the ray-traced point lights (sharp, shadowed, coloured).
    // In the non-RT build torch_voxel_scale()=1 and block_lights()=0 (unchanged look).
    let torch = vec3<f32>(1.0, 0.82, 0.55) * block * torch_voxel_scale()
        + block_lights(in.world_pos, geo_n, in.clip_position.xy);
    let cloud_shade = cloud_shadow(in.world_pos, overcast);
    // Wetness. Only where the sky can actually reach the surface — the underside
    // of an overhang and anything indoors stays dry — and biased to up-facing
    // faces, since that is where water collects.
    // Exposure to the weather, before deciding what form it takes.
    let exposure = overcast * clamp(sky * 1.2, 0.0, 1.0) * mix(0.3, 1.0, max(geo_n.y, 0.0));
    // Snow does not wet anything: below freezing the water arrives as a solid
    // and settles on top instead of soaking in or pooling. Driving wetness from
    // the weather alone gave snowfields puddles and a wet sheen.
    // `/weather snow` overrides the biome so snow is reachable in a local world,
    // where every column is the default plains biome.
    let snowy = max(in.snowy, lighting.extra.w);
    let wetness = exposure * (1.0 - snowy);
    // Snow settles on surfaces the sky reaches and that face up, deepening with
    // the same accumulator the puddles use — it is the same "how long has this
    // been going on" quantity.
    // Capped at half. A full blend replaces the block texture outright, which
    // reads as the terrain having been repainted rather than dusted — at 0.5 the
    // surface underneath still shows through.
    let snow_max = 0.5;
    let snow_cover = snowy
        * lighting.extra.z
        * clamp(sky * 8.0 - 7.0, 0.0, 1.0)
        * smoothstep(0.35, 0.85, geo_n.y)
        * (0.55 + 0.45 * cloud_noise(absolute_pos(in.world_pos).xz * 0.6))
        * snow_max;
    // The accumulator gates the mask; `wetness` only says the surface is exposed.
    let puddles = puddle_mask(in.world_pos, sky, geo_n.y, lighting.extra.z * step(0.01, wetness));
    // Porosity. The vanilla path has no per-material data, so this takes the mid
    // value a pack-less surface falls back to; with a resource pack's specular
    // map it would come from there (rough => porous). It drives BOTH halves of
    // the response, which is what keeps wet stone and wet dirt from looking the
    // same: darkening scales with porosity, gloss scales against it.
    // Fresh snow is a very high-albedo, slightly blue-white diffuser. Mixing the
    // albedo toward it (rather than adding light) keeps it reading as a surface.
    let snow_albedo = vec3<f32>(0.90, 0.93, 0.98);
    let porosity = 0.5;
    // Damp ground darkens a little; standing water darkens a lot more. Driving
    // both from `max(wetness, puddles)` — as this first did — made them
    // IDENTICAL: on open ground wetness is already 1.0, so the puddle mask had
    // no effect on albedo at all, and a puddle differed from wet ground only by
    // its Fresnel term.
    let damp_darkening = 0.18 * wetness;
    let puddle_darkening = (0.26 * porosity + 0.08) * puddles;
    let wet_albedo = 1.0 - clamp(damp_darkening + puddle_darkening, 0.0, 0.75);
    let snow_mix = clamp(snow_cover, 0.0, 1.0);

    if (pbr_on) {
        let view_dir = normalize(lighting.camera_pos.xyz - in.world_pos);
        let spec_tex = textureSample(specular_atlas, pbr_sampler, in.uv);
        let smoothness_tex = spec_tex.r;
        let metallic = spec_tex.g;
        let emissive = spec_tex.b;
        // Wetness, the PBR way: standing water raises smoothness toward a
        // mirror and pins the base reflectance at water's F0 of 0.02, while the
        // albedo darkens. The whole block used to sit in the non-PBR branch
        // BELOW an early `return`, so installing a PBR resource pack made
        // puddles vanish entirely.
        //
        // With a specular map there is real per-material porosity to work from
        // (rough => porous), so wet dirt drinks the light and stays matte while
        // wet stone barely darkens and turns glossy — the material variation the
        // vanilla path has to fake with a constant.
        let porosity_pbr = 0.5 - 0.5 * smoothness_tex;
        let wet_albedo_pbr =
            1.0 - clamp(0.18 * wetness + (0.26 * porosity_pbr + 0.08) * puddles, 0.0, 0.6);
        let albedo_wet = mix(albedo * wet_albedo_pbr, snow_albedo, snow_mix);
        let smoothness = mix(smoothness_tex, 1.0, puddles * sqrt(1.0 - 0.75 * porosity_pbr));
        let roughness = max((1.0 - smoothness) * (1.0 - smoothness), 0.04);
        let f0 = max(mix(vec3<f32>(0.04), albedo_wet, metallic), vec3<f32>(puddles * 0.02));
        let half_dir = normalize(lighting.sun_dir.xyz + view_dir);
        let ndotv = max(dot(n, view_dir), 0.001);
        let ndoth = max(dot(n, half_dir), 0.0);
        let vdoth = max(dot(view_dir, half_dir), 0.0);
        let D = ggx_distribution(ndoth, roughness);
        let G = smith_ggx(ndotv, ndotl, roughness);
        let F = fresnel_schlick(vdoth, f0);
        let spec_brdf = (D * G * F) / max(4.0 * ndotv * ndotl, 0.001);
        let kd = (vec3<f32>(1.0) - F) * (1.0 - metallic);
        let sun_light = lighting.sun_color.rgb
            * (ndotl * shadow * cloud_shade * sun_sky_gate(sky, in.world_pos));
        let ao = clamp(dot(n, geo_n), 0.3, 1.0);
        var lit = (kd * albedo_wet + spec_brdf) * sun_light
            + albedo_wet * light_curve(ambient * ao + torch, gamma);
        let env_spec = f0 * smoothness * smoothness * ambient * ao * sky * 0.5;
        lit = lit + env_spec;
        // Hardware ray-traced reflection (a == 0 in non-RT builds → no change). Reflect
        // the view off the surface and weight by reflectivity: f0 tints metals by their
        // albedo, smoothness sharpens/strengthens it. Adds real mirror-like reflections
        // to polished / metallic blocks. Skipped for near-matte surfaces (no ray cost).
        let reflectivity = max(max(f0.r, f0.g), f0.b) * smoothness;
        if (reflectivity > 0.04) {
            let refl_dir = reflect(-view_dir, n);
            let rt_refl = rt_reflect(in.world_pos + n * 0.05, refl_dir, in.clip_position.xy);
            // Schlick fresnel from f0: a smooth metal reflects ~its own albedo (f0) head-on,
            // rising to 1 at grazing, scaled by smoothness (a single sharp RT ray fakes
            // roughness by weakening rather than blurring). The old f0×smoothness×0.5 left
            // iron/gold barely reflective; this makes them read as real metal.
            let fres = fresnel_schlick(max(dot(n, view_dir), 0.0), f0);
            lit = lit + rt_refl.rgb * fres * smoothness * rt_refl.a;
        }
        lit = lit + albedo_wet * emissive * 3.0;
        return lit;
    }

    let sun = lighting.sun_color.rgb
        * (ndotl * shadow * cloud_shade * sun_sky_gate(sky, in.world_pos));
    let surface = mix(albedo * wet_albedo, snow_albedo, snow_mix);
    var lit = surface * light_curve(ambient + sun + torch, gamma);
    if (lighting.flags.z > 0.5) {
        let view_dir = normalize(lighting.camera_pos.xyz - in.world_pos);
        let half_dir = normalize(lighting.sun_dir.xyz + view_dir);
        let spec = pow(max(dot(n, half_dir), 0.0), 48.0);
        lit = lit + lighting.sun_color.rgb * (spec * shadow * sky * 0.25);
    }
    // Wet sheen: a rained-on surface reflects the SKY, not the sun (under an
    // overcast there is no sun left to reflect). This is the cue that actually
    // connects the rain to the world instead of leaving it painted on top.
    if (wetness > 0.0) {
        let view_dir = normalize(lighting.camera_pos.xyz - in.world_pos);
        // Ripples only disturb standing water, not a merely damp surface.
        var wet_n = n;
        if (puddles > 0.001) {
            let slope = ripple_slope(in.world_pos) * puddles;
            wet_n = normalize(n + vec3<f32>(slope.x, 0.0, slope.y));
        }
        // Schlick against water's F0 of 0.02: almost nothing head-on, almost a
        // mirror at grazing. A flat strength gets both ends wrong.
        let cos_v = max(dot(wet_n, view_dir), 0.0);
        let fres = 0.02 + 0.98 * pow(1.0 - cos_v, 5.0);
        // Water is SMOOTH, so a puddle mirrors the sky well before the view goes
        // grazing. Strict Schlick against F0 = 0.02 only reaches a few percent
        // until then — at the 45-80 degrees a standing player actually views the
        // ground it is 2%, which is why the puddles were invisible. Standing
        // water therefore gets a reflectance FLOOR: a deliberate departure from
        // pure Fresnel, in exchange for reading as water from where the player
        // stands. Damp ground keeps the physical falloff.
        let gloss = mix(fres * wetness * 0.3, max(fres, 0.12), puddles)
            * (1.0 - porosity * 0.5);
        // MIX, not add. Water covers what is beneath it: at grazing angles you
        // see the sky instead of the ground, not the sky on top of it. Adding
        // instead caps the effect at however bright the overcast sky is, which
        // under a storm is dim enough to disappear.
        // No boost: the sky's own radiance is what a puddle reflects. The 1.6x
        // lift here blew the highlights out to white.
        let sky_refl = lighting.fog_color.rgb;
        lit = mix(lit, sky_refl, clamp(gloss, 0.0, 1.0));
    }
    // Self-emissive glow: surfaces of an actual emitter block (per-block flag from the
    // mesher) are pushed to HDR so they overflow into bloom. Using the flag — not a
    // brightness/block-light heuristic — means only the emitter glows, not the warm
    // neighbours its block-light reaches (which produced spurious glow rings).
    if (in.emissive > 0.5) {
        // Modest HDR lift so the emitter blooms but keeps its own colour (a big lift
        // oversaturates the bright core to white — the "wrong colour" look).
        lit = lit + albedo * 0.8;
    }
    return lit;
}

// Distance fog toward the sky horizon colour. Independent of the master shader
// toggle so it can be used on its own.
// Sky radiance in a view direction, for aerial perspective: horizon (fog colour) up to
// a cooler/bluer zenith, warmed toward the sun. Distant terrain fades into THIS rather
// than a flat fog colour, so the haze takes on the sky's direction-dependent tint
// (warm toward the sun, cool away) — the cue that reads as atmospheric depth + scale.
fn aerial_sky(dir: vec3<f32>) -> vec3<f32> {
    let horizon = lighting.fog_color.rgb;
    let zenith = horizon * 0.55 + vec3<f32>(0.10, 0.20, 0.42) * camera.sky_brightness;
    var col = mix(horizon, zenith, smoothstep(0.0, 0.5, dir.y));
    let toward = max(dot(dir, lighting.sun_dir.xyz), 0.0);
    let warm = lighting.sun_color.rgb * 0.8 + horizon * 0.4;
    col = mix(col, warm, pow(toward, 3.0) * 0.45 * camera.sky_brightness);
    return col;
}

fn apply_fog(color: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    if (lighting.fog_params.z < 0.5) {
        return color;
    }
    let to = world_pos - lighting.camera_pos.xyz;
    let dist = length(to);
    let dir = to / max(dist, 1e-4);
    // Softer curve (haze eases in earlier in the mid-range) toward the directional sky.
    let lin = clamp(
        (dist - lighting.fog_params.x) / max(lighting.fog_params.y - lighting.fog_params.x, 0.001),
        0.0,
        1.0,
    );
    let f = pow(lin, 0.75);
    return mix(color, aerial_sky(dir), f);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    let rgb = apply_fog(apply_lighting(albedo_of(texel.rgb, input.color.rgb), input), input.world_pos);
    return vec4<f32>(rgb, texel.a * input.color.a);
}

// Flat-colour variant for the texture-cost benchmark: skips the atlas fetch and
// shades from the vertex colour only (no shader-pack lighting).
@fragment
fn fs_flat(input: VertexOutput) -> @location(0) vec4<f32> {
    let b = day_night(input.light);
    return vec4<f32>(input.color.rgb * b, 1.0);
}

// Depth pre-pass: colour writes are masked off in the pipeline, so this skips
// the texture fetch entirely and just lets the rasterizer record depth. It still
// declares the full VertexOutput input so the inter-stage interface matches
// vs_main (wgpu requires every vertex output to be consumed by the fragment).
@fragment
fn fs_depth(_input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}

@fragment
fn fs_cutout(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    if (texel.a * input.color.a < 0.5) {
        discard;
    }
    let rgb = apply_fog(apply_lighting(albedo_of(texel.rgb, input.color.rgb), input), input.world_pos);
    return vec4<f32>(rgb, 1.0);
}

// Graphics: Fast — the translucent layer (stained glass / ice / water / portal)
// drawn OPAQUE, skipping the blend's destination read. Alpha still has to be
// TESTED even though it is not blended: `glass_pane_top_*` is fully transparent
// outside its two-texel strip, and writing those texels verbatim paints a black
// band along every pane arm. The threshold sits far below stained glass's 0.4
// body alpha, so only genuinely empty texels are dropped.
@fragment
fn fs_opaque_cutout(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    if (texel.a * input.color.a < 0.05) {
        discard;
    }
    let rgb = apply_fog(apply_lighting(albedo_of(texel.rgb, input.color.rgb), input), input.world_pos);
    return vec4<f32>(rgb, 1.0);
}

// Additive glow pass for stained glass: the pane is lit (two-sided) by the ray-traced
// point lights and tinted by the glass colour, so a light behind it makes the WHOLE
// pane glow that colour — a backlit stained-glass window. Drawn additively after the
// alpha-blended pass. Only built on the RT path (block_lights_raw is the live one).
@fragment
fn fs_glass_glow(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    let glass = texel.rgb * input.color.rgb;
    let n = normalize(input.normal);
    let lit = block_lights_raw(input.world_pos, n, input.clip_position.xy);
    // Gentle, clamped glow: enough that a backlit pane reads as lit, but capped so a
    // bright light behind doesn't blow it into a big bloom disc or wash out the glass.
    let glow = min(lit * glass * 0.35, vec3<f32>(0.6));
    return vec4<f32>(glow, 1.0);
}

// G-buffer for the screen-space RT sky-lighting pass: location 0 = geometric normal
// (0.5 + 0.5*n) for an exact, stable per-pixel normal; location 1 = surface albedo
// (texture x tint) so the additive composite can light it (albedo x sky irradiance).
// Solid + cutout variants; cutout alpha-tests so holes write nothing.
struct NormalOut {
    @location(0) normal: vec4<f32>,
    @location(1) albedo: vec4<f32>,
};

@fragment
fn fs_normal(input: VertexOutput) -> NormalOut {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    var o: NormalOut;
    o.normal = vec4<f32>(normalize(input.normal) * 0.5 + 0.5, 1.0);
    o.albedo = vec4<f32>(albedo_of(texel.rgb, input.color.rgb), 1.0);
    return o;
}

@fragment
fn fs_normal_cutout(input: VertexOutput) -> NormalOut {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    if (texel.a * input.color.a < 0.5) {
        discard;
    }
    var o: NormalOut;
    o.normal = vec4<f32>(normalize(input.normal) * 0.5 + 0.5, 1.0);
    o.albedo = vec4<f32>(albedo_of(texel.rgb, input.color.rgb), 1.0);
    return o;
}
