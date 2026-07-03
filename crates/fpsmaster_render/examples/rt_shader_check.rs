//! Headless validation of the ray-tracing render path without launching the game:
//!   1. both chunk-shader variants compile under naga (default rt_stub + the
//!      ray-query rt_common), and
//!   2. the RT solid/cutout pipelines build against a 4-group layout matching the
//!      renderer's (camera / texture / lighting / rt), i.e. the shader's group 3
//!      acceleration-structure + RtParams bindings line up with the pipeline layout.
//!
//! Run on an RT-capable GPU: `cargo run -p fpsmaster_render --example rt_shader_check`.

use fpsmaster_render::ChunkVertex;

const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

fn main() {
    pollster::block_on(run());
}

async fn run() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .expect("no adapter");
    let info = adapter.get_info();
    println!("adapter: {} ({:?}, {:?})", info.name, info.device_type, info.backend);

    let rt = adapter
        .features()
        .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY);
    println!("EXPERIMENTAL_RAY_QUERY supported: {rt}");

    let features = if rt {
        wgpu::Features::EXPERIMENTAL_RAY_QUERY
    } else {
        wgpu::Features::empty()
    };
    let (device, _queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("rt-shader-check"),
            required_features: features,
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: Default::default(),
            // SAFETY: ray queries are used only through the documented safe wgpu API.
            experimental_features: if rt {
                unsafe { wgpu::ExperimentalFeatures::enabled() }
            } else {
                wgpu::ExperimentalFeatures::disabled()
            },
        })
        .await
        .expect("request_device");

    // Default chunk shader: must always compile (no ray_query).
    let default_src = concat!(
        include_str!("../src/shader/rt_stub.wgsl"),
        "\n",
        include_str!("../src/shader/chunk.wgsl"),
    );
    check_module(&device, "chunk (rt_stub)", default_src).await;

    if !rt {
        println!("SKIP rt_common + pipelines: device has no ray-query support");
        println!("ALL CHECKS OK");
        return;
    }

    // Ray-traced chunk shader + the pipelines that use it.
    let rt_src = concat!(
        include_str!("../src/shader/rt_common.wgsl"),
        "\n",
        include_str!("../src/shader/chunk.wgsl"),
    );
    let rt_module = check_module(&device, "chunk (rt_common)", rt_src).await;

    let layout = rt_pipeline_layout(&device);
    for (label, entry) in [("rt-solid", "fs_main"), ("rt-cutout", "fs_cutout")] {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let _pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &rt_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[ChunkVertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &rt_module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(entry == "fs_cutout"),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            cache: None,
            multiview_mask: None,
        });
        if let Some(err) = scope.pop().await {
            panic!("pipeline '{label}' failed validation:\n{err}");
        }
        println!("OK: pipeline {label}");
    }

    println!("ALL CHECKS OK");
}

async fn check_module(device: &wgpu::Device, label: &str, src: &str) -> wgpu::ShaderModule {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    if let Some(err) = scope.pop().await {
        panic!("shader '{label}' failed validation:\n{err}");
    }
    println!("OK: module {label}");
    module
}

/// A 4-group pipeline layout mirroring the renderer's `rt_lit_pipeline_layout`:
/// camera / texture / lighting / rt. Only the binding *types* need to match the
/// shader; the exact resources don't, since we never draw.
fn rt_pipeline_layout(device: &wgpu::Device) -> wgpu::PipelineLayout {
    let uniform = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let tex = |binding, filterable| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let sampler = |binding, ty| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(ty),
        count: None,
    };
    let bgl = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries,
        })
    };
    let camera = bgl("camera", &[uniform(0)]);
    let texture = bgl(
        "texture",
        &[tex(0, true), sampler(1, wgpu::SamplerBindingType::Filtering)],
    );
    let lighting = bgl(
        "lighting",
        &[
            uniform(0),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            sampler(2, wgpu::SamplerBindingType::Comparison),
            tex(3, false),
            tex(4, false),
            sampler(5, wgpu::SamplerBindingType::NonFiltering),
        ],
    );
    let rt = bgl(
        "rt",
        &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::AccelerationStructure {
                    vertex_return: false,
                },
                count: None,
            },
            uniform(1),
        ],
    );
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rt-lit-layout"),
        bind_group_layouts: &[Some(&camera), Some(&texture), Some(&lighting), Some(&rt)],
        immediate_size: 0,
    })
}
