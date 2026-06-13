use std::collections::HashMap;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use recraft_core::{ChunkPos, World};
use thiserror::Error;
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

use crate::{
    build_chunk_mesh,
    chunk_mesh::ChunkNeighborhood,
    mesh_worker::MeshWorker,
    texture::{EntityAtlasImage, TextureAtlasImage, TextureAtlasSource},
    AtlasUv, BiomeColors, Camera, ChunkMesh, Frustum, GuiAtlas, MeshBuffers, ModelMesh,
    ModelVertex, UiFrame, Vertex,
};

#[derive(Debug, Error)]
pub enum RendererError {
    #[error("no compatible GPU adapter found")]
    NoAdapter,
    #[error("request device failed: {0}")]
    RequestDevice(String),
    #[error("surface has no supported formats")]
    NoSurfaceFormat,
    #[error("surface error: {0}")]
    Surface(String),
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

struct GpuBuffers {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

/// A vertex+index buffer pair that persists across frames and is refilled in
/// place with `queue.write_buffer`, only reallocating when the geometry outgrows
/// the current capacity. Used for the per-frame entity/hand geometry so a moving
/// player doesn't allocate two fresh GPU buffers every single frame.
struct DynamicMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    /// Allocated sizes in bytes (>= the bytes currently in use).
    vertex_capacity: u64,
    index_capacity: u64,
    index_count: u32,
}

/// Per-frame timing and draw-scale counters, used to attribute the frame budget
/// to CPU work vs. GPU/swapchain waits while profiling. All times are the wall
/// clock the CPU spent in each region; under an uncapped present mode a large
/// `acquire_us` means the CPU is blocking on the GPU (GPU-bound), while a small
/// one with the bulk of the frame in CPU regions means CPU-bound.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStats {
    /// Microseconds blocked acquiring the next swapchain image.
    pub acquire_us: u32,
    /// Microseconds recording the render passes (CPU command encoding + draw loop).
    pub encode_us: u32,
    /// Microseconds in `queue.submit`.
    pub submit_us: u32,
    /// Microseconds in `frame.present`.
    pub present_us: u32,
    /// Chunk meshes that passed frustum culling this frame.
    pub visible_chunks: u32,
    /// `draw_indexed` calls issued this frame (chunks across all layers + entities).
    pub draw_calls: u32,
    /// Total indices submitted across all chunk draws.
    pub chunk_indices: u32,
}

#[derive(Default)]
struct GpuChunkMesh {
    solid: Option<GpuBuffers>,
    cutout: Option<GpuBuffers>,
    transparent: Option<GpuBuffers>,
}

/// Persistent GPU resources for the UI overlay. The overlay texture is the size
/// of the surface and only re-rasterized/re-uploaded when the `UiFrame` actually
/// changes, instead of allocating and uploading a full-screen texture every
/// frame.
struct UiCache {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    last_frame: UiFrame,
}

pub struct Renderer<'window> {
    surface: wgpu::Surface<'window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    present_modes: Vec<wgpu::PresentMode>,
    size: PhysicalSize<u32>,
    pipeline: wgpu::RenderPipeline,
    cutout_pipeline: wgpu::RenderPipeline,
    item_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    model_pipeline: wgpu::RenderPipeline,
    model_mesh: Option<DynamicMesh>,
    /// First-person held-item geometry (block-atlas textured), per frame.
    first_person_item: Option<DynamicMesh>,
    /// Dropped-item entities in the world (block-atlas textured), per frame.
    world_items: Option<DynamicMesh>,
    /// Crack overlay over the block being mined (vanilla destroy_stage_N).
    break_overlay: Option<DynamicMesh>,
    last_break_overlay: Option<(i32, i32, i32, u8)>,
    entity_bind_group: wgpu::BindGroup,
    /// The entity atlas texture, retained so downloaded player skins can be
    /// written into their rows at runtime (the bind group references it by view).
    entity_texture: wgpu::Texture,
    sky_pipeline: wgpu::RenderPipeline,
    ui_pipeline: wgpu::RenderPipeline,
    ui_bind_group_layout: wgpu::BindGroupLayout,
    ui_sampler: wgpu::Sampler,
    ui_cache: Option<UiCache>,
    gui_atlas: GuiAtlas,
    depth_view: wgpu::TextureView,
    texture_bind_group: wgpu::BindGroup,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    chunk_meshes: HashMap<ChunkPos, GpuChunkMesh>,
    chunk_mesh_generations: HashMap<ChunkPos, u64>,
    next_chunk_mesh_generation: u64,
    biome_colors: BiomeColors,
    atlas_uv: AtlasUv,
    mesh_worker: MeshWorker,
    last_stats: RenderStats,
}

impl<'window> Renderer<'window> {
    pub async fn new(window: &'window Window) -> Result<Self, RendererError> {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window)
            .map_err(|err| RendererError::Surface(err.to_string()))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(RendererError::NoAdapter)?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("recraft-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(|err| RendererError::RequestDevice(err.to_string()))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or(RendererError::NoSurfaceFormat)?;
        let present_modes = caps.present_modes.clone();
        log::info!("surface present modes available: {present_modes:?}");
        let present_mode = pick_present_mode(&present_modes, false);
        let alpha_mode = caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let camera_uniform = CameraUniform {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera-buffer"),
            contents: bytemuck::bytes_of(&camera_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-layout"),
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
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bind-group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let (texture_layout, texture_bind_group, biome_colors, atlas_uv, block_image) =
            create_texture_bind_group(&device, &queue);
        let (entity_texture_layout, entity_bind_group, entity_texture) =
            create_entity_texture_bind_group(&device, &queue);
        // The UI reuses the block atlas (and its name→tile map) for item icons.
        let gui_atlas = GuiAtlas::load(block_image, atlas_uv.clone());
        // Background chunk-meshing pool (built from the shared atlas/biome data).
        let mesh_worker = MeshWorker::new(atlas_uv.clone(), biome_colors);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("chunk-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/chunk.wgsl").into()),
        });
        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/sky.wgsl").into()),
        });
        let ui_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/ui.wgsl").into()),
        });
        let ui_bind_group_layout = create_ui_bind_group_layout(&device);
        let ui_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui-overlay-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("chunk-pipeline-layout"),
            bind_group_layouts: &[&camera_layout, &texture_layout],
            push_constant_ranges: &[],
        });
        let depth_view = create_depth_view(&device, &config);

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chunk-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
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
        // Cutout (leaves/plants/glass): alpha-tested via fs_cutout, fully opaque
        // where kept, writes depth so it occludes correctly. No back-face cull
        // so cross-shaped plants render from both sides.
        let cutout_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chunk-cutout-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_cutout",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
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
        // First-person item sprites use the same alpha test but keep vanilla's
        // item culling. Drawing both sides makes the far side of a 1px-thick
        // sword visible through alpha holes, which looks like two crossed swords.
        let item_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("item-cutout-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_cutout",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
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
        // Translucent (water/ice/stained glass): alpha-blended, tested against the
        // opaque depth buffer but not writing depth (so overlapping translucent
        // faces blend), and not back-face culled.
        let transparent_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chunk-transparent-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        // Untextured colored geometry (entities, first-person hand). Uses only
        // the camera bind group; depth-tested against the world.
        let model_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("model-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/model.wgsl").into()),
        });
        let model_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("model-pipeline-layout"),
                bind_group_layouts: &[&camera_layout, &entity_texture_layout],
                push_constant_ranges: &[],
            });
        let model_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("model-pipeline"),
            layout: Some(&model_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &model_shader,
                entry_point: "vs_main",
                buffers: &[ModelVertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &model_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
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

        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sky_shader,
                entry_point: "vs_main",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &sky_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
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

        let ui_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui-pipeline-layout"),
            bind_group_layouts: &[&ui_bind_group_layout],
            push_constant_ranges: &[],
        });
        let ui_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ui-pipeline"),
            layout: Some(&ui_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &ui_shader,
                entry_point: "vs_main",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &ui_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            present_modes,
            size,
            pipeline,
            cutout_pipeline,
            item_pipeline,
            transparent_pipeline,
            model_pipeline,
            model_mesh: None,
            first_person_item: None,
            world_items: None,
            break_overlay: None,
            last_break_overlay: None,
            entity_bind_group,
            entity_texture,
            sky_pipeline,
            ui_pipeline,
            ui_bind_group_layout,
            ui_sampler,
            ui_cache: None,
            gui_atlas,
            depth_view,
            texture_bind_group,
            camera_buffer,
            camera_bind_group,
            chunk_meshes: HashMap::new(),
            chunk_mesh_generations: HashMap::new(),
            next_chunk_mesh_generation: 1,
            biome_colors,
            atlas_uv,
            mesh_worker,
            last_stats: RenderStats::default(),
        })
    }

    /// Timing/draw-scale counters for the most recently rendered frame. Used by
    /// the app's profiling overlay/log to decide whether the frame is CPU- or
    /// GPU-bound.
    pub fn last_stats(&self) -> RenderStats {
        self.last_stats
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.size = size;
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = create_depth_view(&self.device, &self.config);
    }

    pub fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height.max(1) as f32
    }

    /// Switch between vertical sync (Fifo) and an unsynced present mode
    /// (Mailbox when available, otherwise Immediate). Reconfigures the surface
    /// only when the effective present mode actually changes.
    pub fn set_vsync(&mut self, vsync: bool) {
        let desired = pick_present_mode(&self.present_modes, vsync);
        if self.config.present_mode != desired {
            self.config.present_mode = desired;
            self.surface.configure(&self.device, &self.config);
        }
        log::info!(
            "vsync {} -> present mode {:?}",
            if vsync { "on" } else { "off" },
            self.config.present_mode
        );
        if !vsync && self.config.present_mode == wgpu::PresentMode::Fifo {
            log::warn!(
                "this platform only supports Fifo present mode; vsync cannot be disabled at the surface level (frame cap still applies)"
            );
        }
    }

    pub fn upload_world(&mut self, world: &World) {
        self.chunk_meshes.clear();
        self.chunk_mesh_generations.clear();
        let positions: Vec<_> = world.chunks().map(|chunk| chunk.position).collect();
        self.upload_dirty_chunks(world, positions);
    }

    pub fn upload_dirty_chunks<I>(&mut self, world: &World, positions: I)
    where
        I: IntoIterator<Item = ChunkPos>,
    {
        for pos in positions {
            self.invalidate_chunk_mesh_jobs(pos);
            let mesh = build_chunk_mesh(world, pos, &self.atlas_uv, self.biome_colors);
            self.upload_chunk_mesh(pos, &mesh);
        }
    }

    /// Snapshot the given chunks and queue them for background meshing on the
    /// worker pool. Chunks that are no longer loaded drop their GPU mesh now.
    /// The snapshot clone is the only main-thread cost; the mesh build itself
    /// runs off-thread, so chunk updates never stall the frame.
    pub fn queue_chunk_meshes<I>(&mut self, world: &World, positions: I)
    where
        I: IntoIterator<Item = ChunkPos>,
    {
        for pos in positions {
            match ChunkNeighborhood::snapshot(world, pos) {
                Some(neighborhood) => {
                    let generation = self.invalidate_chunk_mesh_jobs(pos);
                    self.mesh_worker.submit(neighborhood, generation);
                }
                None => {
                    self.invalidate_chunk_mesh_jobs(pos);
                    self.chunk_meshes.remove(&pos);
                }
            }
        }
    }

    /// Upload up to `max` finished background meshes to the GPU. Results for
    /// chunks unloaded since they were queued are discarded.
    pub fn process_ready_meshes(&mut self, world: &World, max: usize) -> usize {
        let mut processed = 0;
        let mut uploaded = 0;
        while processed < max {
            let Some((pos, generation, mesh)) = self.mesh_worker.try_recv() else {
                break;
            };
            processed += 1;
            if self.chunk_mesh_generations.get(&pos).copied() != Some(generation) {
                continue;
            }
            if world.chunk(pos).is_none() {
                self.chunk_meshes.remove(&pos);
            } else {
                self.upload_chunk_mesh(pos, &mesh);
                uploaded += 1;
            }
        }
        uploaded
    }

    fn invalidate_chunk_mesh_jobs(&mut self, pos: ChunkPos) -> u64 {
        let generation = self.next_chunk_mesh_generation;
        self.next_chunk_mesh_generation = self.next_chunk_mesh_generation.wrapping_add(1).max(1);
        self.chunk_mesh_generations.insert(pos, generation);
        generation
    }

    fn upload_chunk_mesh(&mut self, pos: ChunkPos, mesh: &ChunkMesh) {
        if mesh.is_empty() {
            self.chunk_meshes.remove(&pos);
            return;
        }

        self.chunk_meshes.insert(
            pos,
            GpuChunkMesh {
                solid: self.upload_buffers(&mesh.solid, "chunk-solid"),
                cutout: self.upload_buffers(&mesh.cutout, "chunk-cutout"),
                transparent: self.upload_buffers(&mesh.transparent, "chunk-transparent"),
            },
        );
    }

    /// Replace the per-frame entity/hand geometry drawn in the model pass. The
    /// underlying GPU buffers persist across frames and are refilled in place,
    /// only reallocating when the mesh outgrows the current capacity — so the
    /// common case (geometry the same size as last frame) is a single
    /// `write_buffer` per buffer with no allocation.
    pub fn upload_model(&mut self, mesh: &ModelMesh) {
        fill_dynamic_mesh(
            &self.device,
            &self.queue,
            &mut self.model_mesh,
            bytemuck::cast_slice(&mesh.vertices),
            bytemuck::cast_slice(&mesh.indices),
            mesh.indices.len() as u32,
            "model",
        );
    }

    /// The block/item atlas UV table, for building first-person item geometry.
    pub fn atlas_uv(&self) -> &AtlasUv {
        &self.atlas_uv
    }

    /// Replace the first-person held-item geometry (block-atlas textured
    /// `Vertex` data), drawn with the cutout pipeline after the world. Pass
    /// empty slices to hide it.
    pub fn set_first_person_item(&mut self, vertices: &[Vertex], indices: &[u32]) {
        fill_dynamic_mesh(
            &self.device,
            &self.queue,
            &mut self.first_person_item,
            bytemuck::cast_slice(vertices),
            bytemuck::cast_slice(indices),
            indices.len() as u32,
            "first-person-item",
        );
    }

    /// Replace the dropped-item world geometry (block-atlas textured `Vertex`
    /// data), drawn in the world with depth testing. Pass empty slices to clear.
    pub fn set_world_items(&mut self, vertices: &[Vertex], indices: &[u32]) {
        fill_dynamic_mesh(
            &self.device,
            &self.queue,
            &mut self.world_items,
            bytemuck::cast_slice(vertices),
            bytemuck::cast_slice(indices),
            indices.len() as u32,
            "world-items",
        );
    }

    /// Write a normalized 64×64 RGBA player skin into per-player skin row
    /// `row` of the entity atlas. The bind group references the texture by
    /// view, so the in-place write needs no bind-group rebuild.
    pub fn upload_player_skin(&mut self, row: u32, rgba: &[u8]) {
        let px = crate::texture::ENTITY_SLOT_PX;
        let expected = (px * px * 4) as usize;
        if rgba.len() != expected || row >= crate::texture::PLAYER_SKIN_SLOTS {
            return;
        }
        let (x, y) = crate::texture::player_skin_row_origin(row);
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.entity_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * px),
                rows_per_image: Some(px),
            },
            wgpu::Extent3d {
                width: px,
                height: px,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Set (or clear) the mining crack overlay: the block cell being mined and
    /// its 0..9 destroy stage. Rebuilds the small overlay mesh only when the
    /// target or stage actually changes.
    pub fn set_break_overlay(&mut self, overlay: Option<(i32, i32, i32, u8)>) {
        if overlay == self.last_break_overlay {
            return;
        }
        self.last_break_overlay = overlay;
        match overlay {
            None => {
                if let Some(mesh) = &mut self.break_overlay {
                    mesh.index_count = 0;
                }
            }
            Some((x, y, z, stage)) => {
                let (vertices, indices) = break_overlay_geometry(x, y, z, stage, &self.atlas_uv);
                fill_dynamic_mesh(
                    &self.device,
                    &self.queue,
                    &mut self.break_overlay,
                    bytemuck::cast_slice(&vertices),
                    bytemuck::cast_slice(&indices),
                    indices.len() as u32,
                    "break-overlay",
                );
            }
        }
    }

    fn upload_buffers(&self, buffers: &MeshBuffers, label: &str) -> Option<GpuBuffers> {
        if buffers.is_empty() {
            return None;
        }
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&buffers.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&buffers.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        Some(GpuBuffers {
            vertex_buffer,
            index_buffer,
            index_count: buffers.indices.len() as u32,
        })
    }

    pub fn render(&mut self, camera: &Camera) -> Result<(), RendererError> {
        self.render_with_optional_ui(camera, None)
    }

    pub fn render_with_ui(&mut self, camera: &Camera, ui: &UiFrame) -> Result<(), RendererError> {
        self.render_with_optional_ui(camera, Some(ui))
    }

    fn render_with_optional_ui(
        &mut self,
        camera: &Camera,
        ui: Option<&UiFrame>,
    ) -> Result<(), RendererError> {
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&CameraUniform {
                view_proj: camera.view_projection().to_cols_array_2d(),
            }),
        );

        // Time the swapchain acquire separately: under an uncapped present mode a
        // large acquire wait means the CPU is blocking on the GPU (GPU-bound).
        let t_acquire = Instant::now();
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(err) => return Err(RendererError::Surface(err.to_string())),
        };
        let acquire_us = t_acquire.elapsed().as_micros() as u32;

        let t_encode = Instant::now();
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        let mut visible_chunks = 0u32;
        let mut draw_calls = 0u32;
        let mut chunk_indices = 0u32;

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-render-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.52,
                            g: 0.72,
                            b: 1.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.texture_bind_group, &[]);
            pass.draw(0..3, 0..1);

            if !self.chunk_meshes.is_empty() {
                let frustum = camera.frustum();
                // Draw each render layer by iterating the chunk map in place — no
                // per-frame Vec allocation for the visible set. The frustum test
                // is a handful of dot products, cheaper than the heap traffic it
                // replaces, and is re-run per layer (3x) which is negligible.
                //
                // Opaque pass, then alpha-tested cutout (leaves/plants), then the
                // alpha-blended translucent pass (water/glass) last.
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(1, &self.texture_bind_group, &[]);
                for (pos, mesh) in &self.chunk_meshes {
                    if !chunk_in_frustum(&frustum, *pos) {
                        continue;
                    }
                    visible_chunks += 1;
                    if let Some(indices) = draw_buffers(&mut pass, mesh.solid.as_ref()) {
                        draw_calls += 1;
                        chunk_indices += indices;
                    }
                }

                pass.set_pipeline(&self.cutout_pipeline);
                for (pos, mesh) in &self.chunk_meshes {
                    if !chunk_in_frustum(&frustum, *pos) {
                        continue;
                    }
                    if let Some(indices) = draw_buffers(&mut pass, mesh.cutout.as_ref()) {
                        draw_calls += 1;
                        chunk_indices += indices;
                    }
                }

                pass.set_pipeline(&self.transparent_pipeline);
                for (pos, mesh) in &self.chunk_meshes {
                    if !chunk_in_frustum(&frustum, *pos) {
                        continue;
                    }
                    if let Some(indices) = draw_buffers(&mut pass, mesh.transparent.as_ref()) {
                        draw_calls += 1;
                        chunk_indices += indices;
                    }
                }
            }

            // Mining crack overlay: drawn with the translucent pipeline (alpha
            // blended, depth-tested, no depth write) so the crack texels darken
            // the mined block in place.
            if let Some(overlay) = self.break_overlay.as_ref().filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.transparent_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(1, &self.texture_bind_group, &[]);
                pass.set_vertex_buffer(0, overlay.vertex_buffer.slice(..));
                pass.set_index_buffer(overlay.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..overlay.index_count, 0, 0..1);
                draw_calls += 1;
            }

            // Entities and the first-person hand.
            if let Some(model) = self.model_mesh.as_ref().filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.model_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(1, &self.entity_bind_group, &[]);
                pass.set_vertex_buffer(0, model.vertex_buffer.slice(..));
                pass.set_index_buffer(model.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..model.index_count, 0, 0..1);
                draw_calls += 1;
            }

            // Dropped items in the world, textured from the block/item atlas.
            if let Some(items) = self.world_items.as_ref().filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.item_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(1, &self.texture_bind_group, &[]);
                pass.set_vertex_buffer(0, items.vertex_buffer.slice(..));
                pass.set_index_buffer(items.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..items.index_count, 0, 0..1);
                draw_calls += 1;
            }

            // First-person held item, textured from the block/item atlas.
            if let Some(item) = self.first_person_item.as_ref().filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.item_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(1, &self.texture_bind_group, &[]);
                pass.set_vertex_buffer(0, item.vertex_buffer.slice(..));
                pass.set_index_buffer(item.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..item.index_count, 0, 0..1);
                draw_calls += 1;
            }
        }

        if let Some(ui) = ui.filter(|ui| !ui.is_empty()) {
            self.prepare_ui(ui);
            if let Some(cache) = &self.ui_cache {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("ui-render-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    occlusion_query_set: None,
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.ui_pipeline);
                pass.set_bind_group(0, &cache.bind_group, &[]);
                pass.draw(0..3, 0..1);
                draw_calls += 1;
            }
        }
        let encode_us = t_encode.elapsed().as_micros() as u32;

        let t_submit = Instant::now();
        self.queue.submit(Some(encoder.finish()));
        let submit_us = t_submit.elapsed().as_micros() as u32;

        let t_present = Instant::now();
        frame.present();
        let present_us = t_present.elapsed().as_micros() as u32;

        self.last_stats = RenderStats {
            acquire_us,
            encode_us,
            submit_us,
            present_us,
            visible_chunks,
            draw_calls,
            chunk_indices,
        };
        Ok(())
    }

    /// Ensure `self.ui_cache` holds a GPU texture matching `ui`. The texture is
    /// re-created only when the surface size changes and re-rasterized/uploaded
    /// only when the `UiFrame` content differs from the last upload, so a static
    /// HUD costs nothing per frame. The buffer is rasterized at GUI resolution
    /// (window size / gui pixel scale) and upscaled nearest by the UI pass —
    /// the vanilla chunky look at a fraction of the CPU/upload cost.
    fn prepare_ui(&mut self, ui: &UiFrame) {
        // Rasterize at HALF the GUI scale (2 buffer px per GUI px): the
        // unicode font pages draw 16px CJK glyphs into 8 GUI px, so a
        // 1px-per-GUI-px buffer would drop half their rows/columns. Two
        // buffer px per GUI px keeps them 1:1 while still rasterizing at a
        // quarter of the full-resolution cost.
        let scale = crate::ui::gui_pixel_scale(self.config.height).max(1);
        let divisor = (scale / 2).max(1);
        let width = self.config.width.div_ceil(divisor).max(1);
        let height = self.config.height.div_ceil(divisor).max(1);
        let needs_new_texture = self
            .ui_cache
            .as_ref()
            .is_none_or(|cache| cache.width != width || cache.height != height);

        if needs_new_texture {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ui-overlay-texture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ui-overlay-bind-group"),
                layout: &self.ui_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.ui_sampler),
                    },
                ],
            });
            self.ui_cache = Some(UiCache {
                texture,
                bind_group,
                width,
                height,
                // Force the upload below by storing a frame that won't match.
                last_frame: UiFrame::new(),
            });
        }

        let cache = self.ui_cache.as_mut().expect("ui cache just set");
        if !needs_new_texture && cache.last_frame == *ui {
            return;
        }

        let pixels = ui.rasterize(width, height, divisor, &self.gui_atlas);
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &cache.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        cache.last_frame = ui.clone();
    }
}

/// Bind and draw a chunk-layer buffer pair, returning the index count drawn (for
/// profiling) or `None` when there was nothing to draw.
fn draw_buffers<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    buffers: Option<&'a GpuBuffers>,
) -> Option<u32> {
    let buffers = buffers?;
    pass.set_vertex_buffer(0, buffers.vertex_buffer.slice(..));
    pass.set_index_buffer(buffers.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
    pass.draw_indexed(0..buffers.index_count, 0, 0..1);
    Some(buffers.index_count)
}

/// Refill a persistent vertex+index buffer pair in place, reallocating (with
/// headroom) only when the geometry outgrows the current capacity. An
/// `index_count` of 0 keeps the buffers and just stops drawing.
#[allow(clippy::too_many_arguments)]
fn fill_dynamic_mesh(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &mut Option<DynamicMesh>,
    vertices: &[u8],
    indices: &[u8],
    index_count: u32,
    label: &str,
) {
    if index_count == 0 {
        if let Some(mesh) = slot {
            mesh.index_count = 0;
        }
        return;
    }

    let need_vertex = vertices.len() as u64;
    let need_index = indices.len() as u64;
    let fits = slot
        .as_ref()
        .is_some_and(|m| m.vertex_capacity >= need_vertex && m.index_capacity >= need_index);
    if !fits {
        // Grow with headroom so steady churn around a size doesn't reallocate
        // every frame; round up to the 4-byte copy alignment write_buffer needs.
        let vertex_capacity = grow_capacity(need_vertex);
        let index_capacity = grow_capacity(need_index);
        let vertex_label = format!("{label}-vertices");
        let index_label = format!("{label}-indices");
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(vertex_label.as_str()),
            size: vertex_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(index_label.as_str()),
            size: index_capacity,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some(DynamicMesh {
            vertex_buffer,
            index_buffer,
            vertex_capacity,
            index_capacity,
            index_count,
        });
    }

    let mesh = slot.as_mut().expect("dynamic mesh just ensured");
    queue.write_buffer(&mesh.vertex_buffer, 0, vertices);
    queue.write_buffer(&mesh.index_buffer, 0, indices);
    mesh.index_count = index_count;
}

/// Geometry for the mining crack overlay: a cube slightly inflated around the
/// mined block cell, every face textured with the `destroy_stage_<stage>`
/// atlas tile at full brightness. Drawn with the translucent pipeline, so the
/// crack texels alpha-blend over the block beneath (vanilla look).
fn break_overlay_geometry(
    x: i32,
    y: i32,
    z: i32,
    stage: u8,
    atlas: &AtlasUv,
) -> (Vec<Vertex>, Vec<u32>) {
    let uv = atlas.uv(Some(&format!("destroy_stage_{}", stage.min(9))));
    // Inflate past the block faces so the overlay never z-fights them.
    const PAD: f32 = 0.004;
    let lo = [x as f32 - PAD, y as f32 - PAD, z as f32 - PAD];
    let hi = [
        x as f32 + 1.0 + PAD,
        y as f32 + 1.0 + PAD,
        z as f32 + 1.0 + PAD,
    ];

    // Corner order per face matches the atlas UV order (bottom-left, top-left,
    // top-right, bottom-right); the translucent pipeline doesn't cull, so the
    // winding only needs to be consistent.
    let faces: [[[f32; 3]; 4]; 6] = [
        // -X
        [
            [lo[0], lo[1], hi[2]],
            [lo[0], hi[1], hi[2]],
            [lo[0], hi[1], lo[2]],
            [lo[0], lo[1], lo[2]],
        ],
        // +X
        [
            [hi[0], lo[1], lo[2]],
            [hi[0], hi[1], lo[2]],
            [hi[0], hi[1], hi[2]],
            [hi[0], lo[1], hi[2]],
        ],
        // -Y
        [
            [lo[0], lo[1], lo[2]],
            [lo[0], lo[1], hi[2]],
            [hi[0], lo[1], hi[2]],
            [hi[0], lo[1], lo[2]],
        ],
        // +Y
        [
            [lo[0], hi[1], hi[2]],
            [lo[0], hi[1], lo[2]],
            [hi[0], hi[1], lo[2]],
            [hi[0], hi[1], hi[2]],
        ],
        // -Z
        [
            [lo[0], lo[1], lo[2]],
            [lo[0], hi[1], lo[2]],
            [hi[0], hi[1], lo[2]],
            [hi[0], lo[1], lo[2]],
        ],
        // +Z
        [
            [hi[0], lo[1], hi[2]],
            [hi[0], hi[1], hi[2]],
            [lo[0], hi[1], hi[2]],
            [lo[0], lo[1], hi[2]],
        ],
    ];

    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for corners in faces {
        let base = vertices.len() as u32;
        for (corner, corner_uv) in corners.iter().zip(uv.iter()) {
            vertices.push(Vertex {
                position: *corner,
                color: [1.0, 1.0, 1.0, 1.0],
                uv: *corner_uv,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

/// Round `needed` bytes up to a buffer capacity with ~1.5x headroom, 4-byte
/// aligned and at least 4 bytes, so a dynamic mesh hovering around a size does
/// not reallocate every frame (`queue.write_buffer` needs a 4-aligned range).
fn grow_capacity(needed: u64) -> u64 {
    let with_headroom = needed.saturating_add(needed / 2);
    let aligned = (with_headroom + 3) & !3;
    aligned.max(4)
}

/// Whether any part of a chunk's 16×256×16 column is inside the view frustum.
fn chunk_in_frustum(frustum: &Frustum, pos: ChunkPos) -> bool {
    let min = Vec3::new((pos.x * 16) as f32, 0.0, (pos.z * 16) as f32);
    let max = min + Vec3::new(16.0, 256.0, 16.0);
    frustum.intersects_aabb(min, max)
}

/// Choose a present mode for the requested vsync state. `Fifo` is guaranteed to
/// be supported and gives true vertical sync; when vsync is disabled we prefer
/// `Mailbox` (low latency, no tearing) and fall back to `Immediate` then `Fifo`.
fn pick_present_mode(present_modes: &[wgpu::PresentMode], vsync: bool) -> wgpu::PresentMode {
    if vsync {
        // Fifo is always supported and caps to the refresh rate.
        wgpu::PresentMode::Fifo
    } else if present_modes.contains(&wgpu::PresentMode::Immediate) {
        // True no-vsync: uncapped, may tear.
        wgpu::PresentMode::Immediate
    } else if present_modes.contains(&wgpu::PresentMode::Mailbox) {
        // No tearing, render loop not blocked (FPS can exceed refresh).
        wgpu::PresentMode::Mailbox
    } else {
        // Only Fifo available — cannot disable vsync on this platform.
        wgpu::PresentMode::Fifo
    }
}

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

fn create_depth_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth-texture"),
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
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Upload the entity texture atlas (player skin + white texel) and build the
/// texture+sampler bind group the model pass samples at group 1.
fn create_entity_texture_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::BindGroupLayout, wgpu::BindGroup, wgpu::Texture) {
    let atlas = EntityAtlasImage::load_default();
    if atlas.player_skin_loaded {
        log::info!("loaded entity player skin texture");
    } else {
        log::info!("no entity skin asset found; using procedural skin");
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("entity-atlas-texture"),
        size: wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas.pixels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4 * atlas.width),
            rows_per_image: Some(atlas.height),
        },
        wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("entity-atlas-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("entity-texture-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("entity-texture-bind-group"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    (layout, bind_group, texture)
}

fn create_ui_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ui-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn create_texture_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (
    wgpu::BindGroupLayout,
    wgpu::BindGroup,
    BiomeColors,
    AtlasUv,
    Option<image::RgbaImage>,
) {
    let atlas = TextureAtlasImage::load_default();
    let biome_colors = BiomeColors {
        grass: atlas.grass_color,
        foliage: atlas.foliage_color,
    };
    let atlas_uv = atlas.uv_table();
    match &atlas.source {
        TextureAtlasSource::Directory(path) => {
            log::info!(
                "loaded Minecraft 1.8.9 block atlas from asset directory {}",
                path.display()
            )
        }
        TextureAtlasSource::Archive(path) => {
            log::info!(
                "loaded Minecraft 1.8.9 block atlas from asset archive {}",
                path.display()
            )
        }
        TextureAtlasSource::Fallback => {
            log::warn!("Minecraft 1.8.9 assets were not found; using fallback debug block atlas")
        }
    }

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("block-atlas-texture"),
        size: wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas.pixels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4 * atlas.width),
            rows_per_image: Some(atlas.height),
        },
        wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("block-atlas-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("texture-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("texture-bind-group"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    // Hand the CPU atlas image to the UI for block item thumbnails (reuses this
    // build instead of loading the atlas a second time).
    let block_image = image::RgbaImage::from_raw(atlas.width, atlas.height, atlas.pixels);
    (layout, bind_group, biome_colors, atlas_uv, block_image)
}
