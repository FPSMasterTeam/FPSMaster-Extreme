//! Standalone pure-graphics benchmark — a single rotating, vertex-coloured cube
//! rendered through wgpu with its own window, shader, pipeline and buffers. It
//! shares NOTHING with the game renderer (no texture atlas, no UI, no chunk
//! meshes, no player hand): the number it prints is the raw wgpu + driver +
//! present ceiling of the machine, the bar the engine should be measured against.
//!
//! Run:
//!   cargo run --release -p recraft_render --example gfx_bench
//!
//! Args:
//!   --seconds <f>  auto-exit after N seconds and print the overall average
//!   --fill <n>     draw N extra full-screen passes per frame (pure overdraw) to
//!                  probe fill-rate / memory bandwidth instead of the submission
//!                  floor; default 0 (just the cube = submission/present ceiling)
//!   --vsync        cap to the display refresh (default: uncapped / Immediate)
//!
//! A lone cube has almost no fill, so the default run measures the per-frame
//! submission + present floor (the max FPS the pipeline can reach). Add `--fill`
//! to load the framebuffer and read the fill/bandwidth ceiling at native res.

use std::sync::Arc;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 3],
}

const fn v(pos: [f32; 3], color: [f32; 3]) -> Vertex {
    Vertex { pos, color }
}

// 8 cube corners, each a distinct colour so faces are readable while spinning.
const VERTICES: [Vertex; 8] = [
    v([-1.0, -1.0, -1.0], [0.9, 0.1, 0.1]),
    v([1.0, -1.0, -1.0], [0.1, 0.9, 0.1]),
    v([1.0, 1.0, -1.0], [0.1, 0.1, 0.9]),
    v([-1.0, 1.0, -1.0], [0.9, 0.9, 0.1]),
    v([-1.0, -1.0, 1.0], [0.9, 0.1, 0.9]),
    v([1.0, -1.0, 1.0], [0.1, 0.9, 0.9]),
    v([1.0, 1.0, 1.0], [0.9, 0.9, 0.9]),
    v([-1.0, 1.0, 1.0], [0.2, 0.2, 0.2]),
];

#[rustfmt::skip]
const INDICES: [u16; 36] = [
    0, 1, 2, 0, 2, 3, // -z
    4, 6, 5, 4, 7, 6, // +z
    0, 4, 5, 0, 5, 1, // -y
    3, 2, 6, 3, 6, 7, // +y
    0, 3, 7, 0, 7, 4, // -x
    1, 5, 6, 1, 6, 2, // +x
];

const SHADER: &str = r#"
struct Uniforms { mvp: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs(@location(0) p: vec3<f32>, @location(1) c: vec3<f32>) -> VsOut {
    var o: VsOut;
    o.pos = u.mvp * vec4<f32>(p, 1.0);
    o.color = c;
    return o;
}

@fragment
fn fs(i: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(i.color, 1.0);
}

// Full-screen triangle for the overdraw / fill test (no vertex buffer).
@vertex
fn vfill(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4<f32>(p[vi], 0.0, 1.0);
}

@fragment
fn ffill() -> @location(0) vec4<f32> {
    return vec4<f32>(0.01, 0.0, 0.0, 1.0);
}
"#;

struct Args {
    seconds: Option<f32>,
    fill: u32,
    vsync: bool,
    backend: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        seconds: None,
        fill: 0,
        vsync: false,
        backend: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--seconds" => a.seconds = it.next().and_then(|s| s.parse().ok()),
            "--fill" => a.fill = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            "--vsync" => a.vsync = true,
            "--backend" => a.backend = it.next(),
            _ => {}
        }
    }
    a
}

/// Restrict wgpu to a single graphics backend so each can be benchmarked in
/// isolation. `None` / unknown selects all (wgpu's own preference order).
fn select_backends(name: Option<&str>) -> wgpu::Backends {
    match name.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("vulkan" | "vk") => wgpu::Backends::VULKAN,
        Some("dx12" | "d3d12") => wgpu::Backends::DX12,
        Some("gl" | "opengl" | "gles") => wgpu::Backends::GL,
        Some("metal") => wgpu::Backends::METAL,
        _ => wgpu::Backends::all(),
    }
}

fn create_depth(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: config.width.max(1),
                height: config.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn main() {
    let args = parse_args();
    let event_loop = EventLoop::new().expect("event loop");
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("gfx_bench — pure cube")
            .build(&event_loop)
            .expect("window"),
    );

    let backends = select_backends(args.backend.as_deref());
    println!("requested backends: {backends:?}");
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });
    let surface = instance
        .create_surface(window.clone())
        .expect("surface");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
    }))
    .expect("no GPU adapter");
    let info = adapter.get_info();
    println!("GPU adapter: {} ({:?}, {:?})", info.name, info.device_type, info.backend);

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("gfx_bench-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
        },
        None,
    ))
    .expect("device");

    let caps = surface.get_capabilities(&adapter);
    let format = caps.formats[0];
    let present_mode = if args.vsync {
        wgpu::PresentMode::Fifo
    } else if caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
        wgpu::PresentMode::Immediate
    } else {
        caps.present_modes[0]
    };
    println!("present mode: {present_mode:?} | fill passes/frame: {}", args.fill);

    let size = window.inner_size();
    let mut config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &config);
    let mut depth_view = create_depth(&device, &config);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gfx_bench-shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });

    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cube-vertices"),
        contents: bytemuck::cast_slice(&VERTICES),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cube-indices"),
        contents: bytemuck::cast_slice(&INDICES),
        usage: wgpu::BufferUsages::INDEX,
    });
    let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mvp"),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("u-layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("u-bind"),
        layout: &bind_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: ubuf.as_entire_binding(),
        }],
    });

    let cube_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("cube-layout"),
        bind_group_layouts: &[&bind_layout],
        push_constant_ranges: &[],
    });
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
    };
    let cube_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cube-pipeline"),
        layout: Some(&cube_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs",
            buffers: &[vertex_layout],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs",
            targets: &[Some(format.into())],
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    // Full-screen overdraw pipeline (no vertex buffer, no depth interaction).
    let fill_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("fill-layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });
    let fill_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("fill-pipeline"),
        layout: Some(&fill_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vfill",
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "ffill",
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    let fill_passes = args.fill;
    let start = Instant::now();
    let mut window_start = start;
    let mut window_frames = 0u32;
    let mut total_frames = 0u64;
    let mut finished = false;

    event_loop
        .run(move |event, target| {
            target.set_control_flow(ControlFlow::Poll);
            match event {
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => target.exit(),
                    WindowEvent::Resized(new_size) => {
                        config.width = new_size.width.max(1);
                        config.height = new_size.height.max(1);
                        surface.configure(&device, &config);
                        depth_view = create_depth(&device, &config);
                    }
                    _ => {}
                },
                Event::AboutToWait => {
                    let now = Instant::now();
                    let t = (now - start).as_secs_f32();

                    let aspect = config.width as f32 / config.height as f32;
                    let proj = Mat4::perspective_rh(45f32.to_radians(), aspect, 0.1, 100.0);
                    let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 4.0), Vec3::ZERO, Vec3::Y);
                    let model = Mat4::from_rotation_y(t) * Mat4::from_rotation_x(t * 0.6);
                    let mvp = proj * view * model;
                    queue.write_buffer(&ubuf, 0, bytemuck::cast_slice(&mvp.to_cols_array()));

                    let frame = match surface.get_current_texture() {
                        Ok(f) => f,
                        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                            surface.configure(&device, &config);
                            return;
                        }
                        Err(_) => return,
                    };
                    let view_tex = frame
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default());
                    let mut encoder =
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("frame"),
                        });
                    {
                        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &view_tex,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color {
                                        r: 0.05,
                                        g: 0.06,
                                        b: 0.08,
                                        a: 1.0,
                                    }),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: Some(
                                wgpu::RenderPassDepthStencilAttachment {
                                    view: &depth_view,
                                    depth_ops: Some(wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(1.0),
                                        store: wgpu::StoreOp::Discard,
                                    }),
                                    stencil_ops: None,
                                },
                            ),
                            occlusion_query_set: None,
                            timestamp_writes: None,
                        });
                        pass.set_pipeline(&cube_pipeline);
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.set_vertex_buffer(0, vbuf.slice(..));
                        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint16);
                        pass.draw_indexed(0..INDICES.len() as u32, 0, 0..1);

                        // Optional overdraw to load the framebuffer (fill/bandwidth).
                        if fill_passes > 0 {
                            pass.set_pipeline(&fill_pipeline);
                            for _ in 0..fill_passes {
                                pass.draw(0..3, 0..1);
                            }
                        }
                    }
                    queue.submit(Some(encoder.finish()));
                    frame.present();

                    window_frames += 1;
                    total_frames += 1;
                    let elapsed = (now - window_start).as_secs_f32();
                    if elapsed >= 1.0 {
                        let fps = window_frames as f32 / elapsed;
                        println!(
                            "{fps:.0} fps ({:.3} ms/frame)",
                            1000.0 / fps.max(1.0)
                        );
                        window_frames = 0;
                        window_start = now;
                    }

                    if let Some(limit) = args.seconds {
                        if t >= limit && !finished {
                            finished = true;
                            let avg = total_frames as f32 / t.max(0.001);
                            println!(
                                "=== average over {t:.1}s: {avg:.0} fps ({:.3} ms/frame) ===",
                                1000.0 / avg.max(1.0)
                            );
                            target.exit();
                        }
                    }
                }
                _ => {}
            }
        })
        .expect("run");
}
