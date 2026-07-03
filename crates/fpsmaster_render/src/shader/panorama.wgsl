// Vanilla 1.8 title-screen panorama using a 2D texture array.
//
// Rather than reproduce vanilla's FBO-render-then-paste-back pipeline (which
// bakes a 90° rotation into both the modelview AND the paste quad's UVs), we
// sample the 6 faces directly by world-space ray direction.
//
// Face layout (verified from the source PNGs):
//   panorama_0 = front (+Z)   panorama_1 = right (+X)
//   panorama_2 = back  (-Z)   panorama_3 = left  (-X)
//   panorama_4 = up    (+Y)   panorama_5 = down  (-Y)

struct Uniforms {
    yaw: f32,
    pitch: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0)
var panorama_texture: texture_2d_array<f32>;

@group(0) @binding(1)
var panorama_sampler: sampler;

@group(0) @binding(2)
var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let pos = positions[idx];
    var out: VertexOutput;
    out.position = vec4<f32>(pos, 0.0, 1.0);
    out.uv = vec2<f32>((pos.x + 1.0) * 0.5, 1.0 - (pos.y + 1.0) * 0.5);
    return out;
}

fn rot_x(v: vec3<f32>, a: f32) -> vec3<f32> {
    let c = cos(a); let s = sin(a);
    return vec3<f32>(v.x, v.y * c - v.z * s, v.y * s + v.z * c);
}
fn rot_y(v: vec3<f32>, a: f32) -> vec3<f32> {
    let c = cos(a); let s = sin(a);
    return vec3<f32>(v.x * c + v.z * s, v.y, -v.x * s + v.z * c);
}

// Sample the cube faces given a world-space direction. Returns rgb.
fn sample_cube(dir: vec3<f32>) -> vec3<f32> {
    let ax = abs(dir.x);
    let ay = abs(dir.y);
    let az = abs(dir.z);

    var face: i32;
    var uv: vec2<f32>;

    if (ay >= ax && ay >= az) {
        // Up / down face (±Y)
        if (dir.y > 0.0) {
            // up (+Y) = panorama_4
            face = 4;
            // Looking up: u along +X, v along +Z
            uv = vec2<f32>(dir.x, dir.z) / ay;
        } else {
            // down (-Y) = panorama_5
            face = 5;
            uv = vec2<f32>(dir.x, -dir.z) / ay;
        }
    } else if (az >= ax) {
        // Front / back face (±Z)
        if (dir.z > 0.0) {
            // front (+Z) = panorama_0
            face = 0;
            uv = vec2<f32>(dir.x, -dir.y) / az;
        } else {
            // back (-Z) = panorama_2
            face = 2;
            uv = vec2<f32>(-dir.x, -dir.y) / az;
        }
    } else {
        // Right / left face (±X)
        if (dir.x > 0.0) {
            // right (+X) = panorama_1
            face = 1;
            uv = vec2<f32>(-dir.z, -dir.y) / ax;
        } else {
            // left (-X) = panorama_3
            face = 3;
            uv = vec2<f32>(dir.z, -dir.y) / ax;
        }
    }

    // uv is in [-1,1]; map to [0,1] texture coords.
    let tex = (uv + vec2<f32>(1.0)) * 0.5;
    return textureSample(panorama_texture, panorama_sampler, tex, face).rgb;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tan_half = 1.7320508; // tan(60°), gluPerspective(120)
    let ndc_x = in.uv.x * 2.0 - 1.0;
    let ndc_y = 1.0 - in.uv.y * 2.0;

    // Camera-space ray for a forward-looking (+Z) camera, X right, Y up.
    var dir = normalize(vec3<f32>(ndc_x * tan_half, ndc_y * tan_half, 1.0));

    // Scene pitch (look up/down) then yaw (spin around Y).
    dir = rot_x(dir, uniforms.pitch);
    dir = rot_y(dir, uniforms.yaw);

    var color = sample_cube(dir);

    // Vanilla gradient overlays
    let t = in.uv.y;
    color = mix(color, vec3<f32>(1.0), 0.5 * (1.0 - t));
    color = mix(color, vec3<f32>(0.0), 0.5 * t);

    return vec4<f32>(color, 1.0);
}
