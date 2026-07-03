//! Standalone pure-graphics benchmark — a single rotating, vertex-coloured cube
//! rendered through wgpu with its own shader, pipeline and buffers. It shares
//! NOTHING with the game renderer (no texture atlas, no UI, no chunk meshes, no
//! player hand): the number it prints is the raw wgpu + driver ceiling of the
//! machine, the bar the engine should be measured against.
//!
//! Run:
//!   cargo run --release -p fpsmaster_render --example gfx_bench -- [args]
//!
//! Args:
//!   --seconds <f>   auto-exit after N seconds and print the overall average
//!   --fill <n>      draw N extra full-screen passes per frame (pure overdraw) to
//!                   probe fill-rate / memory bandwidth instead of the floor
//!   --backend <b>   force a backend: vulkan | dx12 | gl | metal (default: auto)
//!   --headless      render off-screen with NO window/surface/present. Runs over
//!                   SSH / non-interactive sessions and measures pure GPU render
//!                   throughput (CPU↔GPU serialized per frame via poll(Wait)).
//!   --width/-h <n>  off-screen resolution for --headless (default 1920x1080;
//!                   set it to your real screen size to compare with the game)
//!   --vsync         (headed only) cap to the display refresh
//!
//! A lone cube has almost no fill, so the default measures the per-frame floor.
//! Add --fill to load the framebuffer and read the fill/bandwidth ceiling.

use std::sync::Arc;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::Window,
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const HEADLESS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 3],
}

const fn v(pos: [f32; 3], color: [f32; 3]) -> Vertex {
    Vertex { pos, color }
}

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
    0, 1, 2, 0, 2, 3,
    4, 6, 5, 4, 7, 6,
    0, 4, 5, 0, 5, 1,
    3, 2, 6, 3, 6, 7,
    0, 3, 7, 0, 7, 4,
    1, 5, 6, 1, 6, 2,
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
    headless: bool,
    width: u32,
    height: u32,
}

fn parse_args() -> Args {
    let mut a = Args {
        seconds: None,
        fill: 0,
        vsync: false,
        backend: None,
        headless: false,
        width: 1920,
        height: 1080,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--seconds" => a.seconds = it.next().and_then(|s| s.parse().ok()),
            "--fill" => a.fill = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            "--vsync" => a.vsync = true,
            "--backend" => a.backend = it.next(),
            "--headless" => a.headless = true,
            "--width" | "-w" => {
                a.width = it.next().and_then(|s| s.parse().ok()).unwrap_or(a.width)
            }
            "--height" | "-h" => {
                a.height = it.next().and_then(|s| s.parse().ok()).unwrap_or(a.height)
            }
            _ => {}
        }
    }
    a
}

fn select_backends(name: Option<&str>) -> wgpu::Backends {
    match name.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("vulkan" | "vk") => wgpu::Backends::VULKAN,
        Some("dx12" | "d3d12") => wgpu::Backends::DX12,
        Some("gl" | "opengl" | "gles") => wgpu::Backends::GL,
        Some("metal") => wgpu::Backends::METAL,
        _ => wgpu::Backends::all(),
    }
}

/// Shared GPU resources, built once for a given colour-target format.
struct Scene {
    cube_pipeline: wgpu::RenderPipeline,
    fill_pipeline: wgpu::RenderPipeline,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    ubuf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

fn build_scene(device: &wgpu::Device, format: wgpu::TextureFormat) -> Scene {
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
        bind_group_layouts: &[Some(&bind_layout)],
        immediate_size: 0,
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
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[vertex_layout],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(format.into())],
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        cache: None,
        multiview_mask: None,
    });
    let fill_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("fill-layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let fill_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("fill-pipeline"),
        layout: Some(&fill_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vfill"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("ffill"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        cache: None,
        multiview_mask: None,
    });
    Scene {
        cube_pipeline,
        fill_pipeline,
        vbuf,
        ibuf,
        ubuf,
        bind_group,
    }
}

fn create_depth(device: &wgpu::Device, w: u32, h: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
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

fn update_mvp(queue: &wgpu::Queue, ubuf: &wgpu::Buffer, t: f32, aspect: f32) {
    let proj = Mat4::perspective_rh(45f32.to_radians(), aspect, 0.1, 100.0);
    let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 4.0), Vec3::ZERO, Vec3::Y);
    let model = Mat4::from_rotation_y(t) * Mat4::from_rotation_x(t * 0.6);
    let mvp = proj * view * model;
    queue.write_buffer(ubuf, 0, bytemuck::cast_slice(&mvp.to_cols_array()));
}

fn record_frame(
    encoder: &mut wgpu::CommandEncoder,
    scene: &Scene,
    color: &wgpu::TextureView,
    depth: &wgpu::TextureView,
    fill_passes: u32,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: color,
            resolve_target: None,
            depth_slice: None,
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
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Discard,
            }),
            stencil_ops: None,
        }),
        occlusion_query_set: None,
        timestamp_writes: None,
    multiview_mask: None,
    });
    pass.set_pipeline(&scene.cube_pipeline);
    pass.set_bind_group(0, &scene.bind_group, &[]);
    pass.set_vertex_buffer(0, scene.vbuf.slice(..));
    pass.set_index_buffer(scene.ibuf.slice(..), wgpu::IndexFormat::Uint16);
    pass.draw_indexed(0..INDICES.len() as u32, 0, 0..1);
    if fill_passes > 0 {
        pass.set_pipeline(&scene.fill_pipeline);
        for _ in 0..fill_passes {
            pass.draw(0..3, 0..1);
        }
    }
}

fn request_adapter(instance: &wgpu::Instance, surface: Option<&wgpu::Surface>) -> wgpu::Adapter {
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: surface,
        force_fallback_adapter: false,
    }))
    .expect("no GPU adapter")
}

fn request_device(adapter: &wgpu::Adapter) -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("gfx_bench-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: Default::default(),
            experimental_features: Default::default(),
        },
    ))
    .expect("device")
}

fn main() {
    let args = parse_args();
    let backends = select_backends(args.backend.as_deref());
    println!("requested backends: {backends:?}");
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    if args.headless {
        run_headless(&instance, &args);
    } else {
        run_headed(instance, args);
    }
}

/// Off-screen render loop: no window, no surface, no present. Works over SSH and
/// isolates pure GPU render throughput (each frame is CPU↔GPU serialized via
/// poll(Wait), so the time is the GPU's per-frame cost).
fn run_headless(instance: &wgpu::Instance, args: &Args) {
    let adapter = request_adapter(instance, None);
    let info = adapter.get_info();
    println!(
        "GPU adapter: {} ({:?}, {:?})",
        info.name, info.device_type, info.backend
    );
    println!(
        "HEADLESS {}x{} | fill passes/frame: {}",
        args.width, args.height, args.fill
    );
    let (device, queue) = request_device(&adapter);
    let scene = build_scene(&device, HEADLESS_FORMAT);
    let color = device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-color"),
            size: wgpu::Extent3d {
                width: args.width.max(1),
                height: args.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HEADLESS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default());
    let depth = create_depth(&device, args.width, args.height);
    let aspect = args.width as f32 / args.height as f32;

    let start = Instant::now();
    let mut window_start = start;
    let mut window_frames = 0u32;
    let mut total_frames = 0u64;
    loop {
        let now = Instant::now();
        let t = (now - start).as_secs_f32();
        update_mvp(&queue, &scene.ubuf, t, aspect);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        record_frame(&mut encoder, &scene, &color, &depth, args.fill);
        queue.submit(Some(encoder.finish()));
        let _ = device.poll(wgpu::PollType::wait_indefinitely());

        window_frames += 1;
        total_frames += 1;
        let elapsed = (Instant::now() - window_start).as_secs_f32();
        if elapsed >= 1.0 {
            let fps = window_frames as f32 / elapsed;
            println!("{fps:.0} fps ({:.3} ms/frame)", 1000.0 / fps.max(1.0));
            window_frames = 0;
            window_start = Instant::now();
        }
        if let Some(limit) = args.seconds {
            if t >= limit {
                let avg = total_frames as f32 / t.max(0.001);
                println!(
                    "=== average over {t:.1}s: {avg:.0} fps ({:.3} ms/frame) ===",
                    1000.0 / avg.max(1.0)
                );
                break;
            }
        }
    }
}

struct BenchApp {
    instance: wgpu::Instance,
    args: Args,
    // Initialized in resumed():
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    scene: Option<Scene>,
    config: Option<wgpu::SurfaceConfiguration>,
    depth_view: Option<wgpu::TextureView>,
    start: Instant,
    window_start: Instant,
    window_frames: u32,
    total_frames: u64,
    finished: bool,
}

impl BenchApp {
    fn new(instance: wgpu::Instance, args: Args) -> Self {
        let now = Instant::now();
        Self {
            instance,
            args,
            window: None,
            surface: None,
            device: None,
            queue: None,
            scene: None,
            config: None,
            depth_view: None,
            start: now,
            window_start: now,
            window_frames: 0,
            total_frames: 0,
            finished: false,
        }
    }
}

impl ApplicationHandler for BenchApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("gfx_bench — pure cube");
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        let surface = self.instance.create_surface(window.clone()).expect("surface");
        let adapter = request_adapter(&self.instance, Some(&surface));
        let info = adapter.get_info();
        println!(
            "GPU adapter: {} ({:?}, {:?})",
            info.name, info.device_type, info.backend
        );
        let (device, queue) = request_device(&adapter);

        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats[0];
        let present_mode = if self.args.vsync {
            wgpu::PresentMode::Fifo
        } else if caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
            wgpu::PresentMode::Immediate
        } else {
            caps.present_modes[0]
        };
        println!(
            "present mode: {present_mode:?} | fill passes/frame: {}",
            self.args.fill
        );

        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
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
        let depth_view = create_depth(&device, config.width, config.height);
        let scene = build_scene(&device, format);

        self.start = Instant::now();
        self.window_start = self.start;
        self.depth_view = Some(depth_view);
        self.config = Some(config);
        self.scene = Some(scene);
        self.device = Some(device);
        self.queue = Some(queue);
        self.surface = Some(surface);
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let (Some(device), Some(surface), Some(config)) =
            (self.device.as_ref(), self.surface.as_ref(), self.config.as_mut())
        else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(new_size) => {
                config.width = new_size.width.max(1);
                config.height = new_size.height.max(1);
                surface.configure(device, config);
                self.depth_view = Some(create_depth(device, config.width, config.height));
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Poll);

        let (Some(device), Some(queue), Some(surface), Some(config), Some(scene), Some(depth_view)) = (
            self.device.as_ref(),
            self.queue.as_ref(),
            self.surface.as_ref(),
            self.config.as_mut(),
            self.scene.as_ref(),
            self.depth_view.as_ref(),
        ) else {
            return;
        };

        let now = Instant::now();
        let t = (now - self.start).as_secs_f32();
        let aspect = config.width as f32 / config.height as f32;
        update_mvp(queue, &scene.ubuf, t, aspect);

        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                surface.configure(device, config);
                return;
            }
            _ => return,
        };
        let view_tex = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        record_frame(&mut encoder, scene, &view_tex, depth_view, self.args.fill);
        queue.submit(Some(encoder.finish()));
        frame.present();

        self.window_frames += 1;
        self.total_frames += 1;
        let elapsed = (now - self.window_start).as_secs_f32();
        if elapsed >= 1.0 {
            let fps = self.window_frames as f32 / elapsed;
            println!("{fps:.0} fps ({:.3} ms/frame)", 1000.0 / fps.max(1.0));
            self.window_frames = 0;
            self.window_start = now;
        }
        if let Some(limit) = self.args.seconds {
            if t >= limit && !self.finished {
                self.finished = true;
                let avg = self.total_frames as f32 / t.max(0.001);
                println!(
                    "=== average over {t:.1}s: {avg:.0} fps ({:.3} ms/frame) ===",
                    1000.0 / avg.max(1.0)
                );
                event_loop.exit();
            }
        }
    }
}

fn run_headed(instance: wgpu::Instance, args: Args) {
    let event_loop = EventLoop::new().expect("event loop");
    let mut bench_app = BenchApp::new(instance, args);
    event_loop.run_app(&mut bench_app).expect("run");
}
