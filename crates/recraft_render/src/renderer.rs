use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use recraft_core::{ChunkPos, SectionPos, World};
use thiserror::Error;
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

use crate::{
    build_section_mesh,
    chunk_mesh::ChunkNeighborhood,
    mesh_worker::MeshWorker,
    texture::{EntityAtlasImage, SkyAtlasImage, TextureAtlasImage, TextureAtlasSource},
    AtlasUv, BiomeColors, Camera, ChunkMesh, ChunkMeshBuffers, ChunkVertex, Frustum, GuiAtlas,
    ModelMesh, ModelVertex, UiFrame, Vertex,
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
    /// Day/night sky-light scale fed to the chunk shader's lightmap.
    sky_brightness: f32,
    /// Seconds since startup, for animated effects (water waves).
    time: f32,
    _pad: [f32; 2],
}

impl CameraUniform {
    fn new(view_proj: [[f32; 4]; 4], sky_brightness: f32, time: f32) -> Self {
        Self {
            view_proj,
            sky_brightness,
            time,
            _pad: [0.0; 2],
        }
    }
}

/// Shader-pack lighting uniform (chunk shader group 2): directional sun +
/// ambient, plus the light-space matrix reserved for the shadow map.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct LightingUniform {
    light_view_proj: [[f32; 4]; 4],
    sun_dir: [f32; 4],
    sun_color: [f32; 4],
    ambient: [f32; 4],
    camera_pos: [f32; 4],
    /// x = master enable, y = shadows, z = specular, w = shadow texel size.
    flags: [f32; 4],
    /// rgb = fog colour (sky horizon), w unused.
    fog_color: [f32; 4],
    /// x = fog start dist, y = fog end dist, z = fog enabled, w unused.
    fog_params: [f32; 4],
}

/// Uniform for the post pass's depth-aware effects (DoF / motion blur):
/// reconstruct world position from depth, and reproject into the previous frame.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct PostCamera {
    inv_view_proj: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
}

/// Uniform for the fullscreen sky-gradient pass: the inverse rotation-only
/// view-projection (to reconstruct a per-pixel view ray) plus the time-of-day
/// gradient and sunset-glow colors.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct SkyUniform {
    inv_view_proj: [[f32; 4]; 4],
    horizon: [f32; 4],
    zenith: [f32; 4],
    /// xyz = world-space sun direction, w = sunset glow strength.
    sun_dir: [f32; 4],
    /// rgb = sunset glow color (w unused).
    sunset: [f32; 4],
    /// xyz = camera world position, w = time (seconds) — for volumetric clouds.
    camera_pos: [f32; 4],
    /// x = clouds enabled, yzw reserved.
    cloud_params: [f32; 4],
}

/// Uniform for the celestial pass (sun/moon/stars): the rotation-only
/// view-projection. Geometry is pre-rotated on the CPU into this frame.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct CelestialUniform {
    view_proj: [[f32; 4]; 4],
}

/// A vertex+index buffer pair that persists across frames and is refilled in
/// place with `queue.write_buffer`, only reallocating when the geometry outgrows
/// the current capacity. Used for the per-frame entity/hand geometry so a moving
/// player doesn't allocate two fresh GPU buffers every single frame, and for the
/// per-section chunk meshes so a re-meshed section (e.g. a placed/broken block)
/// reuses its buffers instead of allocating a fresh pair each rebuild.
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
    /// Microseconds blocked acquiring the next swapchain image. Under an
    /// uncapped present mode this is the CPU's wait on the GPU/compositor; macOS
    /// throttles it to the refresh rate when the window is occluded, so it is
    /// not a reliable GPU-cost measure. Prefer `gpu_us` for that.
    pub acquire_us: u32,
    /// True GPU execution time of the main render pass, in microseconds, from a
    /// timestamp query. Independent of present/occlusion throttling. Zero when
    /// the adapter lacks `TIMESTAMP_QUERY` or no sample is ready yet.
    pub gpu_us: u32,
    /// Microseconds preparing the UI layers (rasterize/upload on change, icon mesh).
    pub prepare_us: u32,
    /// Microseconds recording the render passes (CPU command encoding + draw loop).
    pub encode_us: u32,
    /// Microseconds in `queue.submit`.
    pub submit_us: u32,
    /// Microseconds in `frame.present`.
    pub present_us: u32,
    /// Section meshes that passed frustum culling this frame (counted on the
    /// opaque layer pass).
    pub visible_chunks: u32,
    /// `draw_indexed` calls issued this frame (chunks across all layers + entities).
    pub draw_calls: u32,
    /// Total indices submitted across all chunk draws.
    pub chunk_indices: u32,
}

/// A single `draw_indexed_indirect` command (matches the GPU layout).
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct IndirectCmd {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

/// Where one section's data lives inside a `ChunkLayer` page.
#[derive(Clone, Copy)]
struct LayerSlot {
    page: u16,
    vertex_offset: u32,
    vertex_alloc: u32,
    index_offset: u32,
    index_alloc: u32,
    index_count: u32,
}

const CHUNK_VERTEX_SIZE: u64 = std::mem::size_of::<ChunkVertex>() as u64;
const PAGE_VERTEX_CAP: u32 = 1 << 22; // 4M vertices = 64 MB
const PAGE_INDEX_CAP: u32 = 1 << 23; // 8M indices = 16 MB

struct ChunkPage {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    vertex_cursor: u32,
    index_cursor: u32,
}

/// One render layer's paged mega-buffer. Sections are distributed across
/// fixed-size pages (64 MB vertex, 16 MB index each) to stay within GPU
/// buffer-size limits while still batching draws.
struct ChunkLayer {
    pages: Vec<ChunkPage>,
    slots: HashMap<SectionPos, LayerSlot>,
    label: &'static str,
}

impl ChunkLayer {
    fn new(label: &'static str) -> Self {
        Self {
            pages: Vec::new(),
            slots: HashMap::new(),
            label,
        }
    }

    fn create_page(device: &wgpu::Device, label: &str) -> ChunkPage {
        ChunkPage {
            vertex_buf: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: PAGE_VERTEX_CAP as u64 * CHUNK_VERTEX_SIZE,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            index_buf: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: PAGE_INDEX_CAP as u64 * 2,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            vertex_cursor: 0,
            index_cursor: 0,
        }
    }

    fn insert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pos: SectionPos,
        buf: &ChunkMeshBuffers,
    ) {
        if buf.is_empty() {
            self.slots.remove(&pos);
            return;
        }
        let vc = buf.vertices.len() as u32;
        let ic = buf.indices.len() as u32;

        // Try in-place reuse of existing slot.
        if let Some(slot) = self.slots.get_mut(&pos) {
            if vc <= slot.vertex_alloc && ic <= slot.index_alloc {
                let page = &self.pages[slot.page as usize];
                queue.write_buffer(
                    &page.vertex_buf,
                    slot.vertex_offset as u64 * CHUNK_VERTEX_SIZE,
                    bytemuck::cast_slice(&buf.vertices),
                );
                queue.write_buffer(
                    &page.index_buf,
                    slot.index_offset as u64 * 2,
                    bytemuck::cast_slice(&buf.indices),
                );
                slot.index_count = ic;
                return;
            }
            self.slots.remove(&pos);
        }

        // Find a page with enough space, or create one.
        let pi = self
            .pages
            .iter()
            .position(|p| {
                p.vertex_cursor + vc <= PAGE_VERTEX_CAP
                    && p.index_cursor + ic <= PAGE_INDEX_CAP
            })
            .unwrap_or_else(|| {
                self.pages.push(Self::create_page(device, self.label));
                self.pages.len() - 1
            });

        let page = &mut self.pages[pi];
        let vo = page.vertex_cursor;
        let io = page.index_cursor;
        queue.write_buffer(
            &page.vertex_buf,
            vo as u64 * CHUNK_VERTEX_SIZE,
            bytemuck::cast_slice(&buf.vertices),
        );
        queue.write_buffer(
            &page.index_buf,
            io as u64 * 2,
            bytemuck::cast_slice(&buf.indices),
        );
        self.slots.insert(
            pos,
            LayerSlot {
                page: pi as u16,
                vertex_offset: vo,
                vertex_alloc: vc,
                index_offset: io,
                index_alloc: ic,
                index_count: ic,
            },
        );
        page.vertex_cursor += vc;
        page.index_cursor += ic;
    }

    fn remove(&mut self, pos: SectionPos) {
        self.slots.remove(&pos);
    }

    fn clear(&mut self) {
        self.slots.clear();
        for page in &mut self.pages {
            page.vertex_cursor = 0;
            page.index_cursor = 0;
        }
    }
}

struct PageBatch {
    page: usize,
    cmds: Vec<IndirectCmd>,
}

fn collect_layer_batches(
    layer: &ChunkLayer,
    visible: &[SectionPos],
    chunk_indices: &mut u32,
) -> Vec<PageBatch> {
    let mut by_page: HashMap<usize, Vec<IndirectCmd>> = HashMap::new();
    for pos in visible {
        if let Some(s) = layer.slots.get(pos) {
            *chunk_indices += s.index_count;
            by_page.entry(s.page as usize).or_default().push(IndirectCmd {
                index_count: s.index_count,
                instance_count: 1,
                first_index: s.index_offset,
                base_vertex: s.vertex_offset as i32,
                first_instance: 0,
            });
        }
    }
    let mut batches: Vec<PageBatch> = by_page
        .into_iter()
        .map(|(page, cmds)| PageBatch { page, cmds })
        .collect();
    batches.sort_unstable_by_key(|b| b.page);
    batches
}

/// Measures the GPU execution time of the main render pass via a pair of
/// timestamp queries (start/end of pass). The result is read back
/// asynchronously: the pass writes two timestamps, they are resolved into a
/// readback buffer, and `read` maps it a frame or two later. Because the
/// timestamps bracket only on-GPU execution, the figure is independent of the
/// swapchain present/occlusion throttling that inflates `acquire_us`.
struct GpuTimer {
    query_set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// Nanoseconds per timestamp tick (`Queue::get_timestamp_period`).
    period_ns: f32,
    /// Set by the map callback when a readback has completed.
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// A readback is in flight (mapped or awaiting GPU completion); skip
    /// re-arming until it lands.
    pending: bool,
    last_us: u32,
}

/// GPU resources for the title-screen panorama cubemap skybox.
struct PanoramaResources {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
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
    last_commands: Vec<crate::ui::UiCommand>,
}

/// Per-pass draw toggles used to attribute GPU cost on hardware where timestamp
/// queries return nothing (the Intel iGPU): skipping a pass and re-measuring
/// reveals its share. Driven by the `--bench-passes` benchmark via
/// [`Renderer::set_pass_skip`]; all-false in normal runs.
#[derive(Clone, Copy, Default)]
struct DebugSkip {
    sky: bool,
    water: bool,
    ui: bool,
    /// Render the solid layer with a flat colour (no atlas fetch) to isolate
    /// texture-read bandwidth from framebuffer cost.
    flat: bool,
}

pub struct Renderer<'window> {
    surface: wgpu::Surface<'window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    present_modes: Vec<wgpu::PresentMode>,
    size: PhysicalSize<u32>,
    pipeline: wgpu::RenderPipeline,
    /// Depth-only pass for the solid layer: fills the depth buffer first so the
    /// colour pass's early-z skips shading every occluded solid fragment.
    depth_prepass_pipeline: wgpu::RenderPipeline,
    /// Flat-colour solid pipeline (no atlas fetch) for the texture-cost benchmark.
    flat_pipeline: wgpu::RenderPipeline,
    cutout_pipeline: wgpu::RenderPipeline,
    /// No-cull cutout variant for the isometric GUI block-icon cubes.
    gui_cube_pipeline: wgpu::RenderPipeline,
    item_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
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
    panorama: Option<PanoramaResources>,
    sky_uniform_buffer: wgpu::Buffer,
    sky_bind_group: wgpu::BindGroup,
    /// Sun/moon/stars pass: textured quads at infinity drawn after the gradient.
    celestial_pipeline: wgpu::RenderPipeline,
    celestial_uniform_buffer: wgpu::Buffer,
    celestial_uniform_bind_group: wgpu::BindGroup,
    sky_atlas_bind_group: wgpu::BindGroup,
    celestial_mesh: Option<DynamicMesh>,
    /// Pre-generated star quads in the celestial-local frame; rotated and
    /// alpha-faded into `celestial_mesh` each frame.
    star_quads: Vec<[Vec3; 4]>,
    /// World time in ticks (drives the day/night cycle); 6000 (noon) until the
    /// server sends a time update.
    world_time: f64,
    ui_pipeline: wgpu::RenderPipeline,
    ui_bind_group_layout: wgpu::BindGroupLayout,
    ui_sampler: wgpu::Sampler,
    ui_cache: Option<UiCache>,
    /// Foreground UI layer (counts, hover, carried stack) drawn over the 3D
    /// block-icon cube pass.
    ui_overlay_cache: Option<UiCache>,
    /// Identity view-projection so the GUI cube pass can take pre-baked clip
    /// coordinates straight through the shared shader.
    gui_camera_bind_group: wgpu::BindGroup,
    /// 3D block-icon geometry for this frame (block atlas textured, clip-space).
    gui_item_mesh: Option<DynamicMesh>,
    /// World-space billboarded player nametags (vanilla `drawNameplate`): the
    /// rasterized-name texture, its bind group/sampler, the quad mesh and the
    /// caches that avoid re-rasterizing a static name set.
    nametag_mesh: Option<DynamicMesh>,
    nametag_texture: wgpu::Texture,
    nametag_bind_group: wgpu::BindGroup,
    nametag_sampler: wgpu::Sampler,
    nametag_tex_size: (u32, u32),
    nametag_last_names: Vec<String>,
    /// The model pass's group-1 layout (texture+sampler), retained so the
    /// nametag bind group can be rebuilt when its texture grows.
    model_texture_layout: wgpu::BindGroupLayout,
    gui_atlas: GuiAtlas,
    depth_view: wgpu::TextureView,
    /// Full-window depth target for the final UI pass (which draws to the
    /// swapchain after the off-screen world composite, so bloom never blooms the
    /// HUD). Sized to the swapchain, independent of the world render scale.
    window_depth_view: wgpu::TextureView,
    /// Block atlas bound once per mipmap level (index = active level); the
    /// "Mipmaps" option selects which to bind.
    texture_bind_groups: Vec<wgpu::BindGroup>,
    /// Opaque (REPLACE-blend) water/glass pipeline for Graphics: Fast — skips
    /// the alpha-blend destination read.
    water_opaque_pipeline: wgpu::RenderPipeline,
    /// Dedicated water-surface pipeline: animated waves + fresnel reflection.
    water_pipeline: wgpu::RenderPipeline,
    /// Startup instant, for animated effects (water).
    start_time: Instant,
    /// Settings: 3D render-resolution scale, fancy graphics, mipmap level.
    render_scale: f32,
    fancy_graphics: bool,
    mipmap_levels: u32,
    /// Shader-pack lighting uniform + bind group (chunk group 2), and which
    /// sub-effects are enabled.
    lighting_buffer: wgpu::Buffer,
    lighting_bind_group: wgpu::BindGroup,
    shaders_enabled: bool,
    shadows_enabled: bool,
    specular_enabled: bool,
    fog_enabled: bool,
    /// Overall lighting brightness multiplier (user "Brightness" option).
    brightness: f32,
    /// Post-process effect toggles (consumed by the post pass).
    vignette_enabled: bool,
    chromatic_enabled: bool,
    dof_enabled: bool,
    motion_blur_enabled: bool,
    auto_exposure_enabled: bool,
    clouds_enabled: bool,
    /// Sun shadow-map target + the depth-only pipelines that fill it.
    shadow_view: wgpu::TextureView,
    shadow_solid_pipeline: wgpu::RenderPipeline,
    shadow_cutout_pipeline: wgpu::RenderPipeline,
    shadow_uniform_buffer: wgpu::Buffer,
    shadow_uniform_bind_group: wgpu::BindGroup,
    /// Linear HDR off-screen world target (always present): the world renders
    /// here, then the post pass tone-maps it to the sRGB swapchain. Sized by the
    /// render scale.
    offscreen_view: Option<wgpu::TextureView>,
    /// The off-screen texture itself, copied to `scene_copy` before the water
    /// pass so water can sample the opaque scene for screen-space reflection.
    offscreen_tex: Option<wgpu::Texture>,
    scene_copy_tex: Option<wgpu::Texture>,
    /// Water SSR bind group (scene copy + depth + camera); rebuilt with targets.
    water_ssr_layout: wgpu::BindGroupLayout,
    water_ssr_bind_group: Option<wgpu::BindGroup>,
    /// Post pass: HDR scene -> ACES tone-map + grade (+ optional bloom, + upscale).
    bloom_enabled: bool,
    post_pipeline: wgpu::RenderPipeline,
    post_layout: wgpu::BindGroupLayout,
    post_sampler: wgpu::Sampler,
    post_params_buffer: wgpu::Buffer,
    /// Camera uniform for the post pass's depth-aware effects (DoF / motion blur).
    post_camera_buffer: wgpu::Buffer,
    post_bind_group: Option<wgpu::BindGroup>,
    /// Previous frame's view-projection, for motion-blur reprojection.
    prev_view_proj: Mat4,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    chunk_solid: ChunkLayer,
    chunk_cutout: ChunkLayer,
    chunk_transparent: ChunkLayer,
    /// Water surfaces, drawn with the dedicated water pipeline (waves/reflection).
    chunk_water: ChunkLayer,
    chunk_sections: HashSet<SectionPos>,
    indirect_buf: wgpu::Buffer,
    multi_draw: bool,
    chunk_mesh_generations: HashMap<SectionPos, u64>,
    next_chunk_mesh_generation: u64,
    biome_colors: BiomeColors,
    atlas_uv: AtlasUv,
    mesh_worker: MeshWorker,
    /// Optional GPU-time profiler; `None` when the adapter lacks timestamp queries.
    gpu_timer: Option<GpuTimer>,
    /// Whether to measure per-frame GPU time. Off by default (the readback costs
    /// ~0.04 ms/frame); enabled only while the F3 overlay or a benchmark needs it.
    gpu_timing_enabled: bool,
    last_stats: RenderStats,
    /// Temporary per-pass skip toggles for profiling (RECRAFT_SKIP env var).
    debug_skip: DebugSkip,
}

impl<'window> Renderer<'window> {
    pub async fn new(window: &'window Window) -> Result<Self, RendererError> {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window)
            .map_err(|err| RendererError::Surface(err.to_string()))?;
        // Prefer a real hardware GPU. When none is available (e.g. inside a VM
        // without 3D acceleration), fall back to a software adapter (WARP on
        // DX12) so the app still launches, just slowly.
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
        {
            Some(adapter) => adapter,
            None => {
                log::warn!("no hardware GPU adapter found, falling back to software rendering");
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: Some(&surface),
                        force_fallback_adapter: true,
                    })
                    .await
                    .ok_or(RendererError::NoAdapter)?
            }
        };

        let info = adapter.get_info();
        log::info!(
            "GPU adapter: {} ({:?}, {:?})",
            info.name,
            info.device_type,
            info.backend
        );

        // Request timestamp queries when the adapter supports them, for the
        // occlusion-independent GPU-time profiler. Falls back cleanly otherwise.
        let optional_features = adapter.features()
            & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::MULTI_DRAW_INDIRECT);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("recraft-device"),
                    required_features: optional_features,
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(|err| RendererError::RequestDevice(err.to_string()))?;
        let timestamps_enabled = optional_features.contains(wgpu::Features::TIMESTAMP_QUERY);
        let multi_draw = optional_features.contains(wgpu::Features::MULTI_DRAW_INDIRECT);
        log::info!("GPU timestamp queries: {timestamps_enabled}");
        log::info!("multi-draw-indirect: {multi_draw}");

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or(RendererError::NoSurfaceFormat)?;
        // World geometry renders into a linear HDR off-screen target so the post
        // pass can tone-map (ACES) instead of clipping at 1.0; only the final
        // composite + UI pipelines write the sRGB swapchain. Every world-pass
        // pipeline below uses `format` (HDR); UI/post use `surface_format`.
        let format = HDR_FORMAT;
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
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let camera_uniform = CameraUniform::new(Mat4::IDENTITY.to_cols_array_2d(), 1.0, 0.0);
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera-buffer"),
            contents: bytemuck::bytes_of(&camera_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // The chunk fragment shader reads `camera.sky_brightness` for the
                // day/night lightmap, so the camera uniform must be visible in the
                // fragment stage as well as the vertex stage.
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        // The GUI cube pass bakes its isometric transform straight into clip
        // space, so it draws through the shared shader with an identity camera.
        let gui_camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("gui-camera-buffer"),
            contents: bytemuck::bytes_of(&CameraUniform::new(
                Mat4::IDENTITY.to_cols_array_2d(),
                1.0,
                0.0,
            )),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let gui_camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gui-camera-bind-group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: gui_camera_buffer.as_entire_binding(),
            }],
        });
        let (texture_layout, texture_bind_groups, biome_colors, atlas_uv, block_image) =
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
        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/overlay.wgsl").into()),
        });
        let water_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("water-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/water.wgsl").into()),
        });
        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/sky.wgsl").into()),
        });
        let celestial_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("celestial-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/celestial.wgsl").into()),
        });

        // Sky-gradient pass: a single uniform (inverse view + gradient colors).
        let sky_uniform_layout = create_uniform_layout(
            &device,
            "sky-uniform-layout",
            wgpu::ShaderStages::VERTEX_FRAGMENT,
        );
        let sky_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sky-uniform"),
            size: std::mem::size_of::<SkyUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sky_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sky-bind-group"),
            layout: &sky_uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: sky_uniform_buffer.as_entire_binding(),
            }],
        });

        // Celestial pass (sun/moon/stars): a rotation-only view-projection at
        // group 0 and the sky atlas (sun/moon/white texel) at group 1.
        let celestial_uniform_layout =
            create_uniform_layout(&device, "celestial-uniform-layout", wgpu::ShaderStages::VERTEX);
        let celestial_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("celestial-uniform"),
            size: std::mem::size_of::<CelestialUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let celestial_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("celestial-uniform-bind-group"),
            layout: &celestial_uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: celestial_uniform_buffer.as_entire_binding(),
            }],
        });
        let sky_atlas_bind_group = create_sky_atlas_bind_group(&device, &queue, &texture_layout);
        let star_quads = sky_geometry::generate_stars();

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
        // Sun shadow map: a depth target rendered from the light's view and
        // sampled (comparison/PCF) by the chunk shader.
        let shadow_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow-map"),
            size: wgpu::Extent3d {
                width: SHADOW_DIM,
                height: SHADOW_DIM,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_view = shadow_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let shadow_compare_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow-compare-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });

        // Shader-pack lighting bind group (chunk group 2): the lighting uniform
        // plus the shadow map + its comparison sampler.
        let lighting_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lighting-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // Vertex too: the water shader reads the enable flag + time
                    // to displace the surface.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let lighting_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lighting-uniform"),
            size: std::mem::size_of::<LightingUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let lighting_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lighting-bind-group"),
            layout: &lighting_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: lighting_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&shadow_compare_sampler),
                },
            ],
        });
        // Chunk colour pipelines also bind the lighting group (group 2).
        let lit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("chunk-lit-pipeline-layout"),
            bind_group_layouts: &[&camera_layout, &texture_layout, &lighting_layout],
            push_constant_ranges: &[],
        });
        // Water SSR group (3): copied opaque scene colour + sampler + world depth
        // + the post camera uniform (inv-VP / eye), for screen-space reflection.
        let water_ssr_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("water-ssr-layout"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let water_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("water-pipeline-layout"),
            bind_group_layouts: &[&camera_layout, &texture_layout, &lighting_layout, &water_ssr_layout],
            push_constant_ranges: &[],
        });
        let depth_view = create_depth_view(&device, &config);
        let window_depth_view = create_depth_view(&device, &config);

        // Solid colour pass. Depth is already filled by the pre-pass below, so it
        // only tests (LessEqual, matching the pre-pass's recorded depth) without
        // writing — early-z then rejects every occluded fragment before shading.
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chunk-pipeline"),
            layout: Some(&lit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[ChunkVertex::layout()],
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
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        // Flat-colour clone of the solid pipeline (no atlas fetch), used only by
        // the `--bench-passes` flat config to measure texture-read cost.
        let flat_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chunk-flat-pipeline"),
            layout: Some(&lit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[ChunkVertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_flat",
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
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        // Depth-only pre-pass for the solid layer: same geometry, colour writes
        // masked off so each visible solid pixel is shaded exactly once by the
        // colour pass above, regardless of chunk draw order. Cheap geometry
        // versus the textured overdraw it removes on fill/bandwidth-bound GPUs.
        let depth_prepass_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chunk-depth-prepass-pipeline"),
            layout: Some(&lit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[ChunkVertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_depth",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::empty(),
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
        // Cutout (leaves/glass): alpha-tested via fs_cutout, fully opaque where
        // kept, writes depth so it occludes correctly. Back-face culled like
        // vanilla, so a transparent cube shows only the faces facing the player
        // (not the far inner faces through it). Cross-shaped plants, which must
        // be seen from both sides, emit a face for each direction in the mesher.
        let cutout_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chunk-cutout-pipeline"),
            layout: Some(&lit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[ChunkVertex::layout()],
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
        // The GUI block-icon cube pass reuses the cutout shader but must NOT
        // cull: its cubes are baked into clip space through the isometric GUI
        // pose, whose handedness isn't guaranteed to match the back-face winding,
        // and a convex cube resolves correctly by depth alone without culling.
        let gui_cube_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("gui-cube-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &overlay_shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &overlay_shader,
                entry_point: "fs_cutout",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
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
                module: &overlay_shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &overlay_shader,
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
        // faces blend). Back-face culled like vanilla so a glass/ice cube shows
        // only the player-facing faces; the mining-crack overlay (drawn with this
        // pipeline) shares the cube winding, so culling leaves the crack on the
        // visible faces only.
        let transparent_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chunk-transparent-pipeline"),
            layout: Some(&lit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[ChunkVertex::layout()],
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
                cull_mode: Some(wgpu::Face::Back),
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
        // Graphics: Fast water/glass — opaque (REPLACE, no blend dst read) and
        // writes depth so it occludes properly. Saves the alpha-blend bandwidth.
        let water_opaque_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chunk-water-opaque-pipeline"),
            layout: Some(&lit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[ChunkVertex::layout()],
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
        // Dedicated water surface: animated waves + Fresnel reflection. Alpha
        // blended, depth-tested but no depth write (like the translucent layer).
        let water_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("water-pipeline"),
            layout: Some(&water_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &water_shader,
                entry_point: "vs_main",
                buffers: &[ChunkVertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &water_shader,
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
                cull_mode: Some(wgpu::Face::Back),
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
        let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &overlay_shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &overlay_shader,
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
                cull_mode: Some(wgpu::Face::Back),
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

        let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky-pipeline-layout"),
            bind_group_layouts: &[&sky_uniform_layout],
            push_constant_ranges: &[],
        });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky-pipeline"),
            layout: Some(&sky_pipeline_layout),
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
                // Drawn after opaque terrain: the sky sits at the far plane
                // (clip z = 1.0), so LessEqual passes only where no block wrote a
                // nearer depth — i.e. the open sky.
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let panorama = create_panorama_resources(&device, &queue, format);

        // Sun/moon/stars: textured quads at infinity (rotation-only view), drawn
        // after the gradient and before terrain. No depth write/test so terrain
        // (drawn later, with depth) occludes them where it stands in front.
        let celestial_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("celestial-pipeline-layout"),
                bind_group_layouts: &[&celestial_uniform_layout, &texture_layout],
                push_constant_ranges: &[],
            });
        let celestial_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("celestial-pipeline"),
            layout: Some(&celestial_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &celestial_shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &celestial_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Vanilla `renderSky` draws the sun/moon/stars additively
                    // (`GL_SRC_ALPHA, GL_ONE`): the sun/moon sprites have an
                    // opaque black background, so additive blending makes the
                    // black add nothing and vanish while the bright disc lights
                    // the sky. Stars carry their fade in the vertex alpha.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
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
                // Pinned to the far plane in the shader and drawn after terrain,
                // so LessEqual lets the world occlude the sun/moon/stars while
                // they still fill the open sky.
                depth_compare: wgpu::CompareFunction::LessEqual,
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
        // The UI draws in its own pass to the sRGB swapchain after the post pass.
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
                    format: surface_format,
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
                depth_compare: wgpu::CompareFunction::Always,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        // Nametag text atlas: a small RGBA texture, rebuilt when names change.
        // It shares the model pass's texture+sampler layout so nametag billboards
        // draw through the model pipeline.
        let nametag_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nametag-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let (nametag_texture, nametag_bind_group) =
            create_nametag_resources(&device, &entity_texture_layout, &nametag_sampler, 1, 1);

        let gpu_timer = timestamps_enabled.then(|| {
            let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("gpu-frame-timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            });
            // Two u64 timestamps = 16 bytes.
            let resolve = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpu-timestamp-resolve"),
                size: 16,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpu-timestamp-readback"),
                size: 16,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            GpuTimer {
                query_set,
                resolve,
                readback,
                period_ns: queue.get_timestamp_period(),
                ready: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                pending: false,
                last_us: 0,
            }
        });

        // Post pass: HDR off-screen scene -> ACES tone-map + grade (+ optional
        // bloom, + upscale) into the sRGB swapchain. Always runs (the world only
        // ever renders to the HDR off-screen target now).
        let post_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("post-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/post.wgsl").into()),
        });
        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post-layout"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // World depth (for DoF / motion blur).
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Post camera uniform (inv-VP, prev-VP, camera pos).
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let post_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let post_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post-params"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post-camera"),
            size: std::mem::size_of::<PostCamera>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post-pipeline-layout"),
            bind_group_layouts: &[&post_layout],
            push_constant_ranges: &[],
        });
        let post_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("post-pipeline"),
            layout: Some(&post_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &post_shader,
                entry_point: "vs_main",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &post_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        // Sun shadow-map pass: render chunk depth from the light's view. Solid is
        // depth-only (no fragment); cutout alpha-tests so leaves cast shaped
        // shadows. Both target only a depth attachment.
        let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader/shadow.wgsl").into()),
        });
        let shadow_uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow-uniform-layout"),
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
        let shadow_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow-uniform-bind-group"),
            layout: &shadow_uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: shadow_uniform_buffer.as_entire_binding(),
            }],
        });
        let shadow_depth_state = wgpu::DepthStencilState {
            format: SHADOW_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let shadow_solid_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow-solid-layout"),
            bind_group_layouts: &[&shadow_uniform_layout],
            push_constant_ranges: &[],
        });
        let shadow_solid_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow-solid-pipeline"),
            layout: Some(&shadow_solid_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: "vs_main",
                buffers: &[ChunkVertex::layout()],
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(shadow_depth_state.clone()),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        let shadow_cutout_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow-cutout-layout"),
            bind_group_layouts: &[&shadow_uniform_layout, &texture_layout],
            push_constant_ranges: &[],
        });
        let shadow_cutout_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow-cutout-pipeline"),
            layout: Some(&shadow_cutout_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: "vs_main",
                buffers: &[ChunkVertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shadow_shader,
                entry_point: "fs_cutout",
                targets: &[],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(shadow_depth_state),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let chunk_solid = ChunkLayer::new("mega-solid");
        let chunk_cutout = ChunkLayer::new("mega-cutout");
        let chunk_transparent = ChunkLayer::new("mega-transparent");
        let chunk_water = ChunkLayer::new("mega-water");
        let indirect_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("indirect-cmds"),
            size: 4096 * std::mem::size_of::<IndirectCmd>() as u64 * 3,
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
            present_modes,
            size,
            pipeline,
            depth_prepass_pipeline,
            flat_pipeline,
            cutout_pipeline,
            gui_cube_pipeline,
            item_pipeline,
            transparent_pipeline,
            overlay_pipeline,
            model_pipeline,
            model_mesh: None,
            first_person_item: None,
            world_items: None,
            break_overlay: None,
            last_break_overlay: None,
            entity_bind_group,
            entity_texture,
            sky_pipeline,
            panorama,
            sky_uniform_buffer,
            sky_bind_group,
            celestial_pipeline,
            celestial_uniform_buffer,
            celestial_uniform_bind_group,
            sky_atlas_bind_group,
            celestial_mesh: None,
            star_quads,
            world_time: 6000.0,
            ui_pipeline,
            ui_bind_group_layout,
            ui_sampler,
            ui_cache: None,
            ui_overlay_cache: None,
            gui_camera_bind_group,
            gui_item_mesh: None,
            nametag_mesh: None,
            nametag_texture,
            nametag_bind_group,
            nametag_sampler,
            nametag_tex_size: (1, 1),
            nametag_last_names: Vec::new(),
            model_texture_layout: entity_texture_layout,
            gui_atlas,
            depth_view,
            window_depth_view,
            texture_bind_groups,
            water_opaque_pipeline,
            water_pipeline,
            start_time: Instant::now(),
            render_scale: 1.0,
            fancy_graphics: true,
            mipmap_levels: crate::texture::ATLAS_MIP_LEVELS - 1,
            lighting_buffer,
            lighting_bind_group,
            shaders_enabled: false,
            shadows_enabled: false,
            specular_enabled: false,
            fog_enabled: false,
            brightness: 1.0,
            vignette_enabled: false,
            chromatic_enabled: false,
            dof_enabled: false,
            motion_blur_enabled: false,
            auto_exposure_enabled: false,
            clouds_enabled: false,
            shadow_view,
            shadow_solid_pipeline,
            shadow_cutout_pipeline,
            shadow_uniform_buffer,
            shadow_uniform_bind_group,
            offscreen_view: None,
            offscreen_tex: None,
            scene_copy_tex: None,
            water_ssr_layout,
            water_ssr_bind_group: None,
            bloom_enabled: false,
            post_pipeline,
            post_layout,
            post_sampler,
            post_params_buffer,
            post_camera_buffer,
            post_bind_group: None,
            prev_view_proj: Mat4::IDENTITY,
            camera_buffer,
            camera_bind_group,
            chunk_solid,
            chunk_cutout,
            chunk_transparent,
            chunk_water,
            chunk_sections: HashSet::new(),
            indirect_buf,
            multi_draw,
            chunk_mesh_generations: HashMap::new(),
            next_chunk_mesh_generation: 1,
            biome_colors,
            atlas_uv,
            mesh_worker,
            gpu_timer,
            gpu_timing_enabled: false,
            last_stats: RenderStats::default(),
            debug_skip: DebugSkip::default(),
        };
        // The world always renders to the HDR off-screen target; build it now.
        renderer.rebuild_scaled_targets();
        Ok(renderer)
    }

    /// Timing/draw-scale counters for the most recently rendered frame. Used by
    /// the app's profiling overlay/log to decide whether the frame is CPU- or
    /// GPU-bound.
    pub fn last_stats(&self) -> RenderStats {
        self.last_stats
    }

    /// Override the per-pass skip flags at runtime. Used by the in-process pass
    /// benchmark (`--bench-passes`) to A/B individual passes within a single,
    /// thermally-consistent run rather than across separate processes.
    /// Fancy graphics: sky gradient + transparent water. Off → flat sky + opaque
    /// water (cheaper per pixel).
    pub fn set_fancy_graphics(&mut self, on: bool) {
        self.fancy_graphics = on;
    }

    /// Select the block-atlas mipmap level (0 = off; clamped to the built chain).
    /// Swaps which per-level sampler bind group is bound at draw time.
    pub fn set_mipmap_levels(&mut self, levels: u32) {
        self.mipmap_levels = levels.min(crate::texture::ATLAS_MIP_LEVELS - 1);
    }

    /// Master toggle for the shader-pack lighting (directional sun + ambient).
    pub fn set_shaders_enabled(&mut self, on: bool) {
        self.shaders_enabled = on;
    }

    pub fn set_shadows_enabled(&mut self, on: bool) {
        self.shadows_enabled = on;
    }

    pub fn set_specular_enabled(&mut self, on: bool) {
        self.specular_enabled = on;
    }

    /// Distance fog toward the sky horizon colour (independent of the master
    /// shader toggle).
    pub fn set_fog_enabled(&mut self, on: bool) {
        self.fog_enabled = on;
    }

    /// Overall lighting brightness multiplier (vanilla "Brightness" gamma).
    pub fn set_brightness(&mut self, value: f32) {
        self.brightness = value;
    }

    pub fn set_vignette_enabled(&mut self, on: bool) {
        self.vignette_enabled = on;
        self.update_post_params();
    }

    pub fn set_chromatic_enabled(&mut self, on: bool) {
        self.chromatic_enabled = on;
        self.update_post_params();
    }

    pub fn set_dof_enabled(&mut self, on: bool) {
        self.dof_enabled = on;
        self.update_post_params();
    }

    pub fn set_motion_blur_enabled(&mut self, on: bool) {
        self.motion_blur_enabled = on;
        self.update_post_params();
    }

    pub fn set_auto_exposure_enabled(&mut self, on: bool) {
        self.auto_exposure_enabled = on;
        self.update_post_params();
    }

    pub fn set_clouds_enabled(&mut self, on: bool) {
        self.clouds_enabled = on;
    }

    /// Bloom (HDR glow around over-bright pixels). The world already renders to
    /// the HDR off-screen target, so this just toggles the post-pass param.
    pub fn set_bloom_enabled(&mut self, on: bool) {
        if on != self.bloom_enabled {
            self.bloom_enabled = on;
            self.update_post_params();
        }
    }

    /// Set the 3D-world render-resolution scale (0.5..=1.0). Below 1.0 the world
    /// renders to a smaller off-screen target and is upscaled to the window.
    pub fn set_render_scale(&mut self, scale: f32) {
        let scale = scale.clamp(0.25, 1.0);
        if (scale - self.render_scale).abs() > f32::EPSILON {
            self.render_scale = scale;
            self.rebuild_scaled_targets();
        }
    }

    pub fn set_pass_skip(&mut self, sky: bool, water: bool, ui: bool, flat: bool) {
        self.debug_skip = DebugSkip {
            sky,
            water,
            ui,
            flat,
        };
    }

    /// Enable per-frame GPU-time measurement. Off by default because the
    /// timestamp readback costs ~0.04 ms/frame; the app turns it on only while
    /// the F3 overlay is shown or a benchmark is running.
    pub fn set_gpu_timing(&mut self, enabled: bool) {
        self.gpu_timing_enabled = enabled;
    }

    pub fn has_panorama(&self) -> bool {
        self.panorama.is_some()
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.size = size;
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.window_depth_view = create_depth_view(&self.device, &self.config);
        self.rebuild_scaled_targets();
    }

    /// The world render-target size after applying `render_scale`.
    fn scaled_dims(&self) -> (u32, u32) {
        let w = ((self.config.width as f32 * self.render_scale).round() as u32).max(1);
        let h = ((self.config.height as f32 * self.render_scale).round() as u32).max(1);
        (w, h)
    }

    /// Recreate the world depth buffer and the HDR off-screen colour target (both
    /// sized by the render scale) plus the post-pass bind group. The world always
    /// renders off-screen now; the post pass tone-maps it to the swapchain.
    fn rebuild_scaled_targets(&mut self) {
        let (w, h) = self.scaled_dims();
        log::info!(
            "render scale {:.2}: swapchain {}x{} -> world target {}x{} (HDR)",
            self.render_scale,
            self.config.width,
            self.config.height,
            w,
            h
        );
        self.depth_view = create_depth_view_sized(&self.device, w, h);
        let color = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-world-hdr"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = color.create_view(&wgpu::TextureViewDescriptor::default());
        // Copy of the opaque scene the water pass samples for SSR.
        let scene_copy = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene-copy-hdr"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let scene_copy_view = scene_copy.create_view(&wgpu::TextureViewDescriptor::default());
        self.water_ssr_bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water-ssr-bind-group"),
            layout: &self.water_ssr_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&scene_copy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.post_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&self.depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.post_camera_buffer.as_entire_binding(),
                },
            ],
        }));
        self.scene_copy_tex = Some(scene_copy);
        self.post_bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post-bind-group"),
            layout: &self.post_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.post_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.post_params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.post_camera_buffer.as_entire_binding(),
                },
            ],
        }));
        self.offscreen_view = Some(view);
        self.offscreen_tex = Some(color);
        self.update_post_params();
    }

    /// Write the post-pass parameters (bloom + grade) for the current off-screen
    /// size and bloom toggle.
    fn update_post_params(&self) {
        let (w, h) = self.scaled_dims();
        let on = |b: bool, v: f32| if b { v } else { 0.0 };
        // p: bloom threshold, bloom intensity, texel.x, texel.y
        // q: exposure, saturation, contrast, bloom-enabled
        // r: vignette amount, chromatic amount, dof strength, motion-blur strength
        // s: auto-exposure enabled, (reserved), (reserved), (reserved)
        // Neutral grade (sat/contrast 1.0) + slightly-under exposure so sunlit
        // blocks keep texture detail instead of blowing out.
        let params = [
            1.0f32,
            0.6,
            1.0 / w as f32,
            1.0 / h as f32,
            0.85,
            1.0,
            1.0,
            on(self.bloom_enabled, 1.0),
            on(self.vignette_enabled, 0.45),
            on(self.chromatic_enabled, 0.004),
            on(self.dof_enabled, 1.0),
            on(self.motion_blur_enabled, 1.0),
            on(self.auto_exposure_enabled, 1.0),
            0.0,
            0.0,
            0.0,
        ];
        self.queue
            .write_buffer(&self.post_params_buffer, 0, bytemuck::cast_slice(&params));
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
        self.chunk_solid.clear();
        self.chunk_cutout.clear();
        self.chunk_transparent.clear();
        self.chunk_water.clear();
        self.chunk_sections.clear();
        self.chunk_mesh_generations.clear();
        let sections: Vec<SectionPos> = world
            .chunks()
            .flat_map(|chunk| {
                let pos = chunk.position;
                chunk
                    .sections()
                    .map(move |section| SectionPos::new(pos.x, section.y(), pos.z))
            })
            .collect();
        self.upload_dirty_sections(world, sections);
    }

    pub fn upload_dirty_sections<I>(&mut self, world: &World, sections: I)
    where
        I: IntoIterator<Item = SectionPos>,
    {
        for pos in sections {
            self.invalidate_chunk_mesh_jobs(pos);
            let mesh = build_section_mesh(world, pos, &self.atlas_uv, self.biome_colors);
            self.upload_chunk_mesh(pos, &mesh);
        }
    }

    /// Snapshot the given sections and queue them for background meshing on the
    /// worker pool. Sections are grouped by column so each column is snapshotted
    /// (cloned) at most once even when several of its sections are dirty;
    /// sections whose column is no longer loaded drop their GPU mesh now. The
    /// snapshot clone is the only main-thread cost; the mesh build itself runs
    /// off-thread, so chunk updates never stall the frame.
    pub fn queue_chunk_meshes<I>(&mut self, world: &World, sections: I)
    where
        I: IntoIterator<Item = SectionPos>,
    {
        let mut by_column: HashMap<ChunkPos, Vec<i32>> = HashMap::new();
        for pos in sections {
            by_column.entry(pos.chunk()).or_default().push(pos.y);
        }
        for (column, section_ys) in by_column {
            match ChunkNeighborhood::snapshot(world, column) {
                Some(neighborhood) => {
                    let neighborhood = Arc::new(neighborhood);
                    for section_y in section_ys {
                        let pos = SectionPos::new(column.x, section_y, column.z);
                        let generation = self.invalidate_chunk_mesh_jobs(pos);
                        self.mesh_worker
                            .submit(Arc::clone(&neighborhood), section_y, generation);
                    }
                }
                None => {
                    for section_y in section_ys {
                        let pos = SectionPos::new(column.x, section_y, column.z);
                        self.invalidate_chunk_mesh_jobs(pos);
                        self.remove_section(pos);
                    }
                }
            }
        }
    }

    /// Upload up to `max` finished background meshes to the GPU. Results for
    /// sections unloaded since they were queued are discarded.
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
            if world.chunk(pos.chunk()).is_none() {
                self.remove_section(pos);
            } else {
                self.upload_chunk_mesh(pos, &mesh);
                uploaded += 1;
            }
        }
        uploaded
    }

    fn invalidate_chunk_mesh_jobs(&mut self, pos: SectionPos) -> u64 {
        let generation = self.next_chunk_mesh_generation;
        self.next_chunk_mesh_generation = self.next_chunk_mesh_generation.wrapping_add(1).max(1);
        self.chunk_mesh_generations.insert(pos, generation);
        generation
    }

    fn upload_chunk_mesh(&mut self, pos: SectionPos, mesh: &ChunkMesh) {
        if mesh.is_empty() {
            self.remove_section(pos);
            return;
        }
        let device = &self.device;
        let queue = &self.queue;
        self.chunk_solid.insert(device, queue, pos, &mesh.solid);
        self.chunk_cutout.insert(device, queue, pos, &mesh.cutout);
        self.chunk_transparent
            .insert(device, queue, pos, &mesh.transparent);
        self.chunk_water.insert(device, queue, pos, &mesh.water);
        self.chunk_sections.insert(pos);
    }

    fn remove_section(&mut self, pos: SectionPos) {
        self.chunk_solid.remove(pos);
        self.chunk_cutout.remove(pos);
        self.chunk_transparent.remove(pos);
        self.chunk_water.remove(pos);
        self.chunk_sections.remove(&pos);
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

    /// Set the world time in ticks (vanilla 0..24000 per day), which drives the
    /// day/night sky and the world lightmap. Called each frame with partial-tick
    /// interpolation; until the server sends a time update it stays at noon.
    pub fn set_world_time(&mut self, ticks: f64) {
        self.world_time = ticks;
    }

    /// Update the sky-gradient and celestial uniforms/geometry for this frame
    /// from the precomputed `sky` colors and the camera orientation. Run before
    /// the render pass so the buffers are ready when the sky/celestial passes
    /// draw.
    fn prepare_sky(&mut self, camera: &Camera, sky: &crate::sky::SkyColors) {
        // Rotation-only view-projection: sun/moon/stars sit at infinity, so the
        // sky wheels as the camera turns but does not translate with the player.
        let proj = Mat4::perspective_rh(
            camera.fovy_degrees.to_radians(),
            camera.aspect.max(0.001),
            camera.z_near,
            camera.z_far,
        );
        let view_rot = Mat4::look_to_rh(Vec3::ZERO, camera.direction(), Vec3::Y);
        let sky_view_proj = proj * view_rot;
        let inv = sky_view_proj.inverse();
        let sun_dir = crate::sky::celestial_rotation(self.world_time)
            .transform_vector3(Vec3::Y)
            .normalize();

        self.queue.write_buffer(
            &self.sky_uniform_buffer,
            0,
            bytemuck::bytes_of(&SkyUniform {
                inv_view_proj: inv.to_cols_array_2d(),
                horizon: [sky.horizon[0], sky.horizon[1], sky.horizon[2], 1.0],
                zenith: [sky.zenith[0], sky.zenith[1], sky.zenith[2], 1.0],
                sun_dir: [sun_dir.x, sun_dir.y, sun_dir.z, sky.sunset[3]],
                sunset: sky.sunset,
                camera_pos: [
                    camera.position.x,
                    camera.position.y,
                    camera.position.z,
                    self.start_time.elapsed().as_secs_f32(),
                ],
                cloud_params: [if self.clouds_enabled { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
            }),
        );
        self.queue.write_buffer(
            &self.celestial_uniform_buffer,
            0,
            bytemuck::bytes_of(&CelestialUniform {
                view_proj: sky_view_proj.to_cols_array_2d(),
            }),
        );

        let (vertices, indices) =
            sky_geometry::build_mesh(self.world_time, &self.star_quads, sky.star_brightness);
        fill_dynamic_mesh(
            &self.device,
            &self.queue,
            &mut self.celestial_mesh,
            bytemuck::cast_slice(&vertices),
            bytemuck::cast_slice(&indices),
            indices.len() as u32,
            "celestial",
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

    /// Build and upload this frame's billboarded player nametags (vanilla
    /// `EntityRenderer.drawNameplate`): a dark plate with centered white text,
    /// rendered in the world so terrain depth occludes it, facing the camera.
    /// `tags` is (formatted name, head-anchor world position). Pass an empty
    /// slice to hide them.
    pub fn set_nametags(&mut self, camera: &Camera, tags: &[(String, Vec3)]) {
        if tags.is_empty() {
            if let Some(mesh) = &mut self.nametag_mesh {
                mesh.index_count = 0;
            }
            return;
        }
        const SCALE: i32 = 2;
        let line_h = (8 * SCALE) as u32;
        let font = crate::font::font();
        let widths: Vec<u32> = tags
            .iter()
            .map(|(name, _)| font.text_width(name, SCALE).max(1) as u32)
            .collect();
        let tex_w = widths.iter().copied().max().unwrap_or(1).max(2);
        let tex_h = line_h * (tags.len() as u32 + 1);

        // Re-rasterize/upload the name atlas only when the names change; the
        // billboards rebuild every frame because they track the camera.
        let names: Vec<String> = tags.iter().map(|(n, _)| n.clone()).collect();
        if names != self.nametag_last_names || self.nametag_tex_size != (tex_w, tex_h) {
            let mut pixels = vec![0u8; (tex_w * tex_h * 4) as usize];
            // Opaque white texel in the corner for the plate to sample.
            for p in 0..2 {
                for q in 0..2 {
                    let i = ((p * tex_w + q) * 4) as usize;
                    pixels[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
            for (i, (name, _)) in tags.iter().enumerate() {
                let y = (line_h * (i as u32 + 1)) as i32;
                font.draw(
                    &mut pixels, tex_w, tex_h, 0, y, SCALE, [255, 255, 255, 255], false, name,
                );
            }
            if self.nametag_tex_size != (tex_w, tex_h) {
                let (texture, bind_group) = create_nametag_resources(
                    &self.device,
                    &self.model_texture_layout,
                    &self.nametag_sampler,
                    tex_w,
                    tex_h,
                );
                self.nametag_texture = texture;
                self.nametag_bind_group = bind_group;
                self.nametag_tex_size = (tex_w, tex_h);
            }
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.nametag_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * tex_w),
                    rows_per_image: Some(tex_h),
                },
                wgpu::Extent3d {
                    width: tex_w,
                    height: tex_h,
                    depth_or_array_layers: 1,
                },
            );
            self.nametag_last_names = names;
        }

        // Camera-facing billboard basis (matches the dropped-item sprites).
        let forward = camera.direction();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        // Vanilla nameplate scale: 1.6 * 0.016666668 world units per font px.
        let ws = 0.016_666_668 * 1.6 / SCALE as f32;
        let white_uv = [0.5 / tex_w as f32, 0.5 / tex_h as f32];

        let mut vertices: Vec<ModelVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for (i, (_, pos)) in tags.iter().enumerate() {
            let half_w = widths[i] as f32 * ws * 0.5;
            let half_h = line_h as f32 * ws * 0.5;
            let v0 = (line_h * (i as u32 + 1)) as f32 / tex_h as f32;
            let v1 = (line_h * (i as u32 + 2)) as f32 / tex_h as f32;
            let u1 = widths[i] as f32 / tex_w as f32;
            let pad = ws;
            // Plate: pushed slightly away so the text wins the depth test.
            push_billboard(
                &mut vertices,
                &mut indices,
                *pos + forward * 0.01,
                right,
                up,
                half_w + pad,
                half_h + pad,
                [white_uv, white_uv, white_uv, white_uv],
                [0.0, 0.0, 0.0, 0.376],
            );
            // Text (v grows downward in the atlas, so the quad top samples v0).
            push_billboard(
                &mut vertices,
                &mut indices,
                *pos,
                right,
                up,
                half_w,
                half_h,
                [[0.0, v0], [u1, v0], [u1, v1], [0.0, v1]],
                [1.0, 1.0, 1.0, 1.0],
            );
        }
        fill_dynamic_mesh(
            &self.device,
            &self.queue,
            &mut self.nametag_mesh,
            bytemuck::cast_slice(&vertices),
            bytemuck::cast_slice(&indices),
            indices.len() as u32,
            "nametag",
        );
    }


    pub fn render(&mut self, camera: &Camera) -> Result<(), RendererError> {
        self.render_inner(camera, None, None)
    }

    pub fn render_with_ui(&mut self, camera: &Camera, ui: &UiFrame) -> Result<(), RendererError> {
        self.render_inner(camera, Some(ui), None)
    }

    /// Render the title-screen panorama background behind the UI overlay.
    /// `panorama_timer` is the vanilla `panoramaTimer + partialTicks`.
    pub fn render_panorama(&mut self, ui: &UiFrame, panorama_timer: f32) -> Result<(), RendererError> {
        let dummy_camera = Camera::new(Vec3::ZERO, 1.0);
        self.render_inner(&dummy_camera, Some(ui), Some(panorama_timer))
    }

    /// Drive map callbacks and, if a timestamp readback has completed, decode it
    /// into microseconds. Returns the most recent GPU pass time (0 if unknown).
    fn read_gpu_timestamp(&mut self) -> u32 {
        if self.gpu_timer.is_none() {
            return 0;
        }
        // Non-blocking poll so a completed map_async fires its callback.
        self.device.poll(wgpu::Maintain::Poll);
        let timer = self.gpu_timer.as_mut().expect("checked above");
        if timer.ready.swap(false, std::sync::atomic::Ordering::Acquire) {
            {
                let mapped = timer.readback.slice(..).get_mapped_range();
                let t0 = u64::from_le_bytes(mapped[0..8].try_into().unwrap());
                let t1 = u64::from_le_bytes(mapped[8..16].try_into().unwrap());
                let delta = t1.saturating_sub(t0);
                timer.last_us = (delta as f64 * timer.period_ns as f64 / 1000.0) as u32;
            }
            timer.readback.unmap();
            timer.pending = false;
        }
        timer.last_us
    }

    /// Map the timestamp readback buffer to fetch this frame's result later.
    /// Must be called after the resolve/copy has been submitted.
    fn arm_gpu_timestamp(&mut self) {
        let Some(timer) = self.gpu_timer.as_mut() else {
            return;
        };
        if timer.pending {
            return;
        }
        timer.pending = true;
        let ready = timer.ready.clone();
        timer.readback.slice(..).map_async(wgpu::MapMode::Read, move |res| {
            if res.is_ok() {
                ready.store(true, std::sync::atomic::Ordering::Release);
            }
        });
    }

    fn render_inner(
        &mut self,
        camera: &Camera,
        ui: Option<&UiFrame>,
        panorama_timer: Option<f32>,
    ) -> Result<(), RendererError> {
        // Time-of-day sky/lighting: one celestial-math evaluation drives the
        // chunk lightmap (sky_brightness), the sky-gradient colors and the
        // sun/moon/star geometry.
        let sky = crate::sky::sky_colors(self.world_time);
        let view_proj = camera.view_projection();
        let time = self.start_time.elapsed().as_secs_f32();
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&CameraUniform::new(
                view_proj.to_cols_array_2d(),
                sky.sun_brightness,
                time,
            )),
        );
        // Post-pass camera (depth-aware DoF / motion blur): unproject depth and
        // reproject into last frame. Updated before prev is overwritten.
        self.queue.write_buffer(
            &self.post_camera_buffer,
            0,
            bytemuck::bytes_of(&PostCamera {
                inv_view_proj: view_proj.inverse().to_cols_array_2d(),
                prev_view_proj: self.prev_view_proj.to_cols_array_2d(),
                camera_pos: [camera.position.x, camera.position.y, camera.position.z, 0.0],
            }),
        );
        self.prev_view_proj = view_proj;
        self.prepare_sky(camera, &sky);

        // Shader-pack lighting uniform: directional sun (day/night scaled) +
        // ambient, consumed by the chunk fragment when shaders are enabled.
        let sun_dir = crate::sky::celestial_rotation(self.world_time)
            .transform_vector3(Vec3::Y)
            .normalize();
        let sb = sky.sun_brightness;
        let cam = camera.position;
        let on = |b: bool| if b { 1.0 } else { 0.0 };
        let shadows_on = self.shaders_enabled && self.shadows_enabled;
        // Orthographic light frustum centred just ahead of the camera, viewed
        // from the sun direction — covers the near scene the player actually sees.
        let light_view_proj = if shadows_on {
            let center = camera.position + camera.direction() * (SHADOW_RADIUS * 0.5);
            let eye = center + sun_dir * (SHADOW_RADIUS * 2.5);
            let up = if sun_dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
            let view = Mat4::look_at_rh(eye, center, up);
            let proj = Mat4::orthographic_rh(
                -SHADOW_RADIUS,
                SHADOW_RADIUS,
                -SHADOW_RADIUS,
                SHADOW_RADIUS,
                0.1,
                SHADOW_RADIUS * 5.0,
            );
            proj * view
        } else {
            Mat4::IDENTITY
        };
        self.queue.write_buffer(
            &self.shadow_uniform_buffer,
            0,
            bytemuck::bytes_of(&light_view_proj.to_cols_array_2d()),
        );
        self.queue.write_buffer(
            &self.lighting_buffer,
            0,
            bytemuck::bytes_of(&LightingUniform {
                light_view_proj: light_view_proj.to_cols_array_2d(),
                sun_dir: [sun_dir.x, sun_dir.y, sun_dir.z, 0.0],
                sun_color: [1.0 * sb, 0.96 * sb, 0.88 * sb, 0.0],
                ambient: [0.45, 0.48, 0.55, 0.0],
                camera_pos: [cam.x, cam.y, cam.z, 0.0],
                flags: [
                    on(self.shaders_enabled),
                    on(shadows_on),
                    on(self.shaders_enabled && self.specular_enabled),
                    1.0 / SHADOW_DIM as f32,
                ],
                fog_color: [sky.horizon[0], sky.horizon[1], sky.horizon[2], 0.0],
                fog_params: [
                    camera.z_far * 0.45,
                    camera.z_far * 0.92,
                    on(self.fog_enabled),
                    self.brightness,
                ],
            }),
        );

        // GPU-time profiler: collect any completed readback, then decide whether
        // to issue a fresh timestamp pair this frame (only when no readback is in
        // flight, so the readback buffer is free to write).
        let gpu_us = self.read_gpu_timestamp();
        // The timestamp resolve + buffer copy + map_async readback costs ~0.04 ms
        // of CPU per frame, so it only runs when something actually displays the
        // number (the F3 overlay or a benchmark run). Normal gameplay skips it.
        let measure_gpu =
            self.gpu_timing_enabled && self.gpu_timer.as_ref().is_some_and(|t| !t.pending);

        // Prepare the UI layers up front (re-rasterize/upload on change, icon
        // mesh refill) so their draws can join the single frame pass below.
        let t_prepare = Instant::now();
        let ui = ui.filter(|ui| !ui.is_empty());
        if let Some(ui) = ui {
            self.prepare_ui(ui);
        }
        let prepare_us = t_prepare.elapsed().as_micros() as u32;
        // Gate the UI draws on this frame actually having UI: the cached
        // textures persist across frames and must not draw stale overlays.
        let draw_ui = ui.is_some();

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
        let swapchain_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // Render the world into the scaled off-screen target when render_scale
        // < 1.0 (then upscale-blit below); otherwise straight to the swapchain.
        let view: &wgpu::TextureView = self.offscreen_view.as_ref().unwrap_or(&swapchain_view);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        // Sun shadow pass: render every loaded chunk section's depth from the
        // light's view (no frustum cull, so off-screen casters still shadow the
        // visible scene). Per-section draws against the paged mega-buffers.
        if shadows_on {
            let mut sp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            sp.set_bind_group(0, &self.shadow_uniform_bind_group, &[]);
            // Solid casters (depth only).
            sp.set_pipeline(&self.shadow_solid_pipeline);
            for slot in self.chunk_solid.slots.values() {
                let page = &self.chunk_solid.pages[slot.page as usize];
                sp.set_vertex_buffer(0, page.vertex_buf.slice(..));
                sp.set_index_buffer(page.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                sp.draw_indexed(
                    slot.index_offset..slot.index_offset + slot.index_count,
                    slot.vertex_offset as i32,
                    0..1,
                );
            }
            // Cutout casters (alpha-tested) — needs the atlas at group 1.
            sp.set_pipeline(&self.shadow_cutout_pipeline);
            sp.set_bind_group(1, &self.texture_bind_groups[self.mipmap_levels as usize], &[]);
            for slot in self.chunk_cutout.slots.values() {
                let page = &self.chunk_cutout.pages[slot.page as usize];
                sp.set_vertex_buffer(0, page.vertex_buf.slice(..));
                sp.set_index_buffer(page.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                sp.draw_indexed(
                    slot.index_offset..slot.index_offset + slot.index_count,
                    slot.vertex_offset as i32,
                    0..1,
                );
            }
        }

        let mut visible_chunks = 0u32;
        let mut draw_calls = 0u32;
        let mut chunk_indices = 0u32;
        // Water draws (page index + command) captured during collection and
        // issued in a separate pass after the opaque scene is copied, so the
        // water shader can sample it for screen-space reflection.
        let mut water_draws: Vec<(usize, IndirectCmd)> = Vec::new();

        // Bracket the whole frame pass with timestamps when measuring this frame.
        let timestamp_writes = self
            .gpu_timer
            .as_ref()
            .filter(|_| measure_gpu)
            .map(|t| wgpu::RenderPassTimestampWrites {
                query_set: &t.query_set,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: Some(1),
            });

        // When the title screen requests the panorama, upload its rotation
        // uniforms; the pass below draws it instead of the sky+world.
        let panorama_active = panorama_timer.is_some() && self.panorama.is_some();
        if let (Some(timer), Some(pan)) = (panorama_timer, &self.panorama) {
            // Vanilla: pitch = sin(timer/400)*25+20 degrees, yaw = -timer*0.1 degrees.
            let pitch = ((timer / 400.0).sin() * 25.0 + 20.0).to_radians();
            let yaw = (-timer * 0.1).to_radians();
            self.queue.write_buffer(
                &pan.uniform_buffer,
                0,
                bytemuck::bytes_of(&[yaw, pitch, 0.0_f32, 0.0_f32]),
            );
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-render-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // The fullscreen sky gradient overwrites every pixel, so
                        // this clear is only a backstop; use the time-of-day
                        // horizon color so it can never flash a stale blue.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: sky.horizon[0] as f64,
                            g: sky.horizon[1] as f64,
                            b: sky.horizon[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        // Stored (not discarded) so the post pass can sample world
                        // depth for depth-of-field and motion blur.
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes,
            });

            if panorama_active {
                // Title-screen panorama replaces the sky+world entirely.
                let pan = self.panorama.as_ref().expect("checked panorama_active");
                pass.set_pipeline(&pan.pipeline);
                pass.set_bind_group(0, &pan.bind_group, &[]);
                pass.draw(0..3, 0..1);
                draw_calls += 1;
            } else {
            // Opaque + cutout terrain first, so the fullscreen sky shader below
            // can depth-test against them and skip every pixel a block covers
            // (a large fill-rate saving on weak GPUs). The transparent layer is
            // drawn after the sky so water/glass still blends over it.
            let mut cmd_offset = 0u64;
            let cmd_stride = std::mem::size_of::<IndirectCmd>() as u64;
            let trans_batches = if !self.chunk_sections.is_empty() {
                let frustum = camera.frustum();
                let visible: Vec<SectionPos> = self
                    .chunk_sections
                    .iter()
                    .copied()
                    .filter(|pos| section_in_frustum(&frustum, *pos))
                    .collect();
                visible_chunks = visible.len() as u32;

                // Collect per-page indirect commands for each layer.
                let solid_batches =
                    collect_layer_batches(&self.chunk_solid, &visible, &mut chunk_indices);
                let cutout_batches =
                    collect_layer_batches(&self.chunk_cutout, &visible, &mut chunk_indices);
                let trans_batches =
                    collect_layer_batches(&self.chunk_transparent, &visible, &mut chunk_indices);
                // Water is drawn in its own later pass (SSR), not via the shared
                // indirect buffer — capture its draws here.
                let water_batches =
                    collect_layer_batches(&self.chunk_water, &visible, &mut chunk_indices);
                for b in &water_batches {
                    for c in &b.cmds {
                        water_draws.push((b.page, *c));
                    }
                }

                // Pack all commands into the indirect buffer (order: solid,
                // cutout, transparent — matching the draw order so the running
                // cmd_offset lines up).
                let total_cmds: usize = solid_batches.iter().chain(&cutout_batches)
                    .chain(&trans_batches)
                    .map(|b| b.cmds.len()).sum();
                if total_cmds > 0 {
                    let cmd_size = std::mem::size_of::<IndirectCmd>() as u64;
                    let needed = total_cmds as u64 * cmd_size;
                    if needed > self.indirect_buf.size() {
                        self.indirect_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("indirect-cmds"),
                            size: (needed * 2).max(4096),
                            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                    }
                    let mut all_cmds = Vec::with_capacity(total_cmds);
                    for b in solid_batches.iter().chain(&cutout_batches)
                        .chain(&trans_batches) {
                        all_cmds.extend_from_slice(&b.cmds);
                    }
                    self.queue.write_buffer(
                        &self.indirect_buf,
                        0,
                        bytemuck::cast_slice(&all_cmds),
                    );
                }

                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(
                    1,
                    &self.texture_bind_groups[self.mipmap_levels as usize],
                    &[],
                );
                pass.set_bind_group(2, &self.lighting_bind_group, &[]);

                // Depth pre-pass: fill the solid layer's depth first (colour
                // masked off) so the solid colour pass below shades each visible
                // pixel exactly once. A local offset walks the same solid
                // commands; the colour passes keep the running cmd_offset.
                if !solid_batches.is_empty() {
                    pass.set_pipeline(&self.depth_prepass_pipeline);
                    let mut pre_offset = 0u64;
                    for batch in &solid_batches {
                        let page = &self.chunk_solid.pages[batch.page];
                        pass.set_vertex_buffer(0, page.vertex_buf.slice(..));
                        pass.set_index_buffer(page.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                        let count = batch.cmds.len() as u32;
                        if self.multi_draw {
                            pass.multi_draw_indexed_indirect(&self.indirect_buf, pre_offset, count);
                        } else {
                            for c in &batch.cmds {
                                pass.draw_indexed(c.first_index..c.first_index + c.index_count, c.base_vertex, 0..1);
                            }
                        }
                        pre_offset += count as u64 * cmd_stride;
                    }
                }

                if !solid_batches.is_empty() {
                    let solid_pipeline = if self.debug_skip.flat {
                        &self.flat_pipeline
                    } else {
                        &self.pipeline
                    };
                    pass.set_pipeline(solid_pipeline);
                    for batch in &solid_batches {
                        let page = &self.chunk_solid.pages[batch.page];
                        pass.set_vertex_buffer(0, page.vertex_buf.slice(..));
                        pass.set_index_buffer(page.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                        let count = batch.cmds.len() as u32;
                        draw_calls += count;
                        if self.multi_draw {
                            pass.multi_draw_indexed_indirect(&self.indirect_buf, cmd_offset, count);
                        } else {
                            for c in &batch.cmds {
                                pass.draw_indexed(c.first_index..c.first_index + c.index_count, c.base_vertex, 0..1);
                            }
                        }
                        cmd_offset += count as u64 * cmd_stride;
                    }
                }

                if !cutout_batches.is_empty() {
                    pass.set_pipeline(&self.cutout_pipeline);
                    for batch in &cutout_batches {
                        let page = &self.chunk_cutout.pages[batch.page];
                        pass.set_vertex_buffer(0, page.vertex_buf.slice(..));
                        pass.set_index_buffer(page.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                        let count = batch.cmds.len() as u32;
                        draw_calls += count;
                        if self.multi_draw {
                            pass.multi_draw_indexed_indirect(&self.indirect_buf, cmd_offset, count);
                        } else {
                            for c in &batch.cmds {
                                pass.draw_indexed(c.first_index..c.first_index + c.index_count, c.base_vertex, 0..1);
                            }
                        }
                        cmd_offset += count as u64 * cmd_stride;
                    }
                }

                trans_batches
            } else {
                Vec::new()
            };

            // Sky gradient (fullscreen view-ray) then the sun/moon/stars, drawn
            // AFTER the opaque world and depth-tested (LessEqual at the far plane)
            // so the shader runs only on pixels no block covers. Graphics: Fast
            // skips it entirely — the clear already holds the flat horizon colour.
            if !self.debug_skip.sky && self.fancy_graphics {
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(0, &self.sky_bind_group, &[]);
            pass.draw(0..3, 0..1);
            if let Some(mesh) = self.celestial_mesh.as_ref().filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.celestial_pipeline);
                pass.set_bind_group(0, &self.celestial_uniform_bind_group, &[]);
                pass.set_bind_group(1, &self.sky_atlas_bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                draw_calls += 1;
            }
            }

            // Transparent terrain (water/glass) over the sky and opaque world.
            // The sky/celestial draws rebound groups 0/1, so restore them first.
            if !trans_batches.is_empty() && !self.debug_skip.water {
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(
                    1,
                    &self.texture_bind_groups[self.mipmap_levels as usize],
                    &[],
                );
                pass.set_bind_group(2, &self.lighting_bind_group, &[]);
                // Graphics: Fast renders water/glass opaque (no blend dst read).
                pass.set_pipeline(if self.fancy_graphics {
                    &self.transparent_pipeline
                } else {
                    &self.water_opaque_pipeline
                });
                for batch in &trans_batches {
                    let page = &self.chunk_transparent.pages[batch.page];
                    pass.set_vertex_buffer(0, page.vertex_buf.slice(..));
                    pass.set_index_buffer(page.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                    let count = batch.cmds.len() as u32;
                    draw_calls += count;
                    if self.multi_draw {
                        pass.multi_draw_indexed_indirect(&self.indirect_buf, cmd_offset, count);
                    } else {
                        for c in &batch.cmds {
                            pass.draw_indexed(c.first_index..c.first_index + c.index_count, c.base_vertex, 0..1);
                        }
                    }
                    cmd_offset += count as u64 * cmd_stride;
                }
            }

            // Mining crack overlay: drawn with the translucent pipeline (alpha
            // blended, depth-tested, no depth write) so the crack texels darken
            // the mined block in place.
            if let Some(overlay) = self.break_overlay.as_ref().filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.overlay_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(
                    1,
                    &self.texture_bind_groups[self.mipmap_levels as usize],
                    &[],
                );
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

            // Billboarded player nametags (depth-tested against the world).
            if let Some(mesh) = self.nametag_mesh.as_ref().filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.model_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(1, &self.nametag_bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                draw_calls += 1;
            }

            // Dropped items in the world, textured from the block/item atlas.
            if let Some(items) = self.world_items.as_ref().filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.item_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(
                    1,
                    &self.texture_bind_groups[self.mipmap_levels as usize],
                    &[],
                );
                pass.set_vertex_buffer(0, items.vertex_buffer.slice(..));
                pass.set_index_buffer(items.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..items.index_count, 0, 0..1);
                draw_calls += 1;
            }

            // First-person held item, textured from the block/item atlas.
            if let Some(item) = self.first_person_item.as_ref().filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.item_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(
                    1,
                    &self.texture_bind_groups[self.mipmap_levels as usize],
                    &[],
                );
                pass.set_vertex_buffer(0, item.vertex_buffer.slice(..));
                pass.set_index_buffer(item.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..item.index_count, 0, 0..1);
                draw_calls += 1;
            }
            } // else (non-panorama sky + world content)
        }

        // Water pass: reflective water over the now-rendered opaque scene. Fancy
        // graphics copies the scene colour first so the water shader can sample
        // it for screen-space reflection; Fast graphics draws plain water.
        if !water_draws.is_empty() && !self.debug_skip.water {
            let use_ssr = self.fancy_graphics
                && self.offscreen_tex.is_some()
                && self.scene_copy_tex.is_some()
                && self.water_ssr_bind_group.is_some();
            if use_ssr {
                let (w, h) = self.scaled_dims();
                encoder.copy_texture_to_texture(
                    wgpu::ImageCopyTexture {
                        texture: self.offscreen_tex.as_ref().unwrap(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyTexture {
                        texture: self.scene_copy_tex.as_ref().unwrap(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
            }
            let mut wp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("water-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            wp.set_pipeline(if use_ssr {
                &self.water_pipeline
            } else {
                &self.water_opaque_pipeline
            });
            wp.set_bind_group(0, &self.camera_bind_group, &[]);
            wp.set_bind_group(1, &self.texture_bind_groups[self.mipmap_levels as usize], &[]);
            wp.set_bind_group(2, &self.lighting_bind_group, &[]);
            if use_ssr {
                wp.set_bind_group(3, self.water_ssr_bind_group.as_ref().unwrap(), &[]);
            }
            for (page, c) in &water_draws {
                let p = &self.chunk_water.pages[*page];
                wp.set_vertex_buffer(0, p.vertex_buf.slice(..));
                wp.set_index_buffer(p.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                wp.draw_indexed(c.first_index..c.first_index + c.index_count, c.base_vertex, 0..1);
                draw_calls += 1;
            }
        }

        // Post pass: HDR off-screen world → ACES tone-map + grade (+ optional
        // bloom, + upscale) into the sRGB swapchain. Always runs.
        if let Some(bind) = &self.post_bind_group {
            let mut post = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("post-composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &swapchain_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            post.set_pipeline(&self.post_pipeline);
            post.set_bind_group(0, bind, &[]);
            post.draw(0..3, 0..1);
        }

        // UI pass: drawn to the swapchain AFTER the world composite, so the HUD
        // is never fed through bloom/blit. Loads the composited (or directly
        // rendered) world colour and clears its own full-window depth for the 3D
        // block icons. Skipped entirely when this frame has no UI.
        if draw_ui && !self.debug_skip.ui {
            let mut up = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &swapchain_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.window_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            // UI background layer (under the 3D block icons).
            if let Some(cache) = &self.ui_cache {
                up.set_pipeline(&self.ui_pipeline);
                up.set_bind_group(0, &cache.bind_group, &[]);
                up.draw(0..3, 0..1);
                draw_calls += 1;
            }
            // 3D block icons: convex cubes (cull-free) baked into clip space via
            // the identity camera. The pass cleared depth to the far plane, so
            // they depth-test correctly among themselves.
            if let Some(mesh) = self.gui_item_mesh.as_ref().filter(|m| m.index_count > 0) {
                up.set_pipeline(&self.gui_cube_pipeline);
                up.set_bind_group(0, &self.gui_camera_bind_group, &[]);
                up.set_bind_group(1, &self.texture_bind_groups[self.mipmap_levels as usize], &[]);
                up.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                up.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                up.draw_indexed(0..mesh.index_count, 0, 0..1);
                draw_calls += 1;
            }
            // UI foreground layer (counts, hover, carried stack) over the cubes.
            if let Some(cache) = &self.ui_overlay_cache {
                up.set_pipeline(&self.ui_pipeline);
                up.set_bind_group(0, &cache.bind_group, &[]);
                up.draw(0..3, 0..1);
                draw_calls += 1;
            }
        }
        // Resolve this frame's timestamps into the readback buffer (still in the
        // same command buffer, after the pass that wrote them).
        if measure_gpu {
            if let Some(timer) = &self.gpu_timer {
                encoder.resolve_query_set(&timer.query_set, 0..2, &timer.resolve, 0);
                encoder.copy_buffer_to_buffer(&timer.resolve, 0, &timer.readback, 0, 16);
            }
        }
        let encode_us = t_encode.elapsed().as_micros() as u32;

        let t_submit = Instant::now();
        self.queue.submit(Some(encoder.finish()));
        let submit_us = t_submit.elapsed().as_micros() as u32;

        // Kick off the async readback for the timestamps we just submitted.
        if measure_gpu {
            self.arm_gpu_timestamp();
        }

        let t_present = Instant::now();
        frame.present();
        let present_us = t_present.elapsed().as_micros() as u32;

        self.last_stats = RenderStats {
            acquire_us,
            gpu_us,
            prepare_us,
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
        let scale = crate::ui::gui_pixel_scale(self.config.width, self.config.height).max(1);
        let divisor = (scale / 2).max(1);
        let width = self.config.width.div_ceil(divisor).max(1);
        let height = self.config.height.div_ceil(divisor).max(1);
        prepare_ui_layer(
            &self.device,
            &self.queue,
            &mut self.ui_cache,
            &self.ui_bind_group_layout,
            &self.ui_sampler,
            &self.gui_atlas,
            ui.back_commands(),
            width,
            height,
            divisor,
            "ui-back",
        );
        prepare_ui_layer(
            &self.device,
            &self.queue,
            &mut self.ui_overlay_cache,
            &self.ui_bind_group_layout,
            &self.ui_sampler,
            &self.gui_atlas,
            ui.overlay_commands(),
            width,
            height,
            divisor,
            "ui-front",
        );

        // 3D block icons, baked into clip space for the GPU cube pass.
        let surface = (self.config.width as f32, self.config.height as f32);
        let mut vertices: Vec<Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for item in ui.block_items() {
            crate::gui_item::append_block_icon(
                &mut vertices,
                &mut indices,
                recraft_core::BlockState::new(item.block_id, item.meta),
                item.dst,
                surface,
                &self.atlas_uv,
                &self.biome_colors,
            );
        }
        fill_dynamic_mesh(
            &self.device,
            &self.queue,
            &mut self.gui_item_mesh,
            bytemuck::cast_slice(&vertices),
            bytemuck::cast_slice(&indices),
            indices.len() as u32,
            "gui-block-items",
        );
    }
}

/// Re-rasterize one UI layer into its cached GPU texture, recreating the texture
/// on a surface resize and re-uploading only when the command list changed.
#[allow(clippy::too_many_arguments)]
fn prepare_ui_layer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cache: &mut Option<UiCache>,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    gui: &GuiAtlas,
    commands: &[crate::ui::UiCommand],
    width: u32,
    height: u32,
    divisor: u32,
    label: &str,
) {
    let needs_new_texture = cache
        .as_ref()
        .is_none_or(|cache| cache.width != width || cache.height != height);
    if needs_new_texture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        *cache = Some(UiCache {
            texture,
            bind_group,
            width,
            height,
            // Force the upload below by storing a list that won't match.
            last_commands: vec![crate::ui::UiCommand::Rect {
                rect: crate::ui::UiRect::new(-1, -1, 0, 0),
                color: crate::ui::UiColor::rgba(0, 0, 0, 0),
            }],
        });
    }

    let cache = cache.as_mut().expect("ui cache just set");
    if !needs_new_texture && cache.last_commands == commands {
        return;
    }

    let pixels = UiFrame::rasterize(commands, width, height, divisor, gui);
    queue.write_texture(
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
    cache.last_commands = commands.to_vec();
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
                light: crate::FULLBRIGHT,
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

/// Whether any part of a 16×16×16 section is inside the view frustum. Unlike the
/// old column test this also culls vertically, so sections under the floor or
/// high above are skipped.
fn section_in_frustum(frustum: &Frustum, pos: SectionPos) -> bool {
    let min = Vec3::new((pos.x * 16) as f32, (pos.y * 16) as f32, (pos.z * 16) as f32);
    let max = min + Vec3::splat(16.0);
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

// Depth32Float (not Depth24Plus) so the post pass can sample the world depth for
// depth-of-field and motion blur (Depth24Plus is not reliably loadable).
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Linear HDR format for the off-screen world target, so the post pass can
/// tone-map highlights (sun/specular/bloom) above 1.0 instead of clipping.
const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Sun shadow map resolution and format (quality-first; cheap on a dGPU).
const SHADOW_DIM: u32 = 2048;
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// World-space half-extent of the shadow frustum centred near the camera.
const SHADOW_RADIUS: f32 = 96.0;

/// A single-binding bind-group layout holding one uniform buffer.
fn create_uniform_layout(
    device: &wgpu::Device,
    label: &str,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// Upload the sky atlas (sun + moon phases + a white star texel) and build the
/// texture+sampler bind group the celestial pass samples at group 1. Reuses the
/// block-atlas (filterable texture + sampler) layout. The texture/sampler are
/// kept alive by the returned bind group, so they need not be stored.
fn create_sky_atlas_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::BindGroup {
    let atlas = SkyAtlasImage::load_default();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sky-atlas-texture"),
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
        label: Some("sky-atlas-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sky-atlas-bind-group"),
        layout,
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
    })
}

/// Sun/moon/star geometry for the celestial pass. Sun and moon are vanilla
/// `renderSky` quads; stars are a fixed random field. All are authored in the
/// celestial-local frame and rotated by `celestial_rotation` here so the pass
/// only needs the rotation-only view-projection.
mod sky_geometry {
    use crate::sky;
    use crate::{Vertex, FULLBRIGHT};
    use crate::texture::{sky_moon_rect, sky_sun_rect, sky_white_uv};
    use glam::Vec3;

    const STAR_COUNT: usize = 1500;
    const STAR_DIST: f32 = 100.0;
    const STAR_SIZE: f32 = 0.65;

    /// Deterministic star field in the celestial-local frame: `STAR_COUNT`
    /// small quads scattered over the sphere, each billboarded around its
    /// outward direction with a random roll. Generated once at startup.
    pub fn generate_stars() -> Vec<[Vec3; 4]> {
        let mut rng = Lcg::new(0x9E3779B9);
        let mut quads = Vec::with_capacity(STAR_COUNT);
        while quads.len() < STAR_COUNT {
            let d = Vec3::new(rng.unit(), rng.unit(), rng.unit());
            let len_sq = d.length_squared();
            if !(0.01..1.0).contains(&len_sq) {
                continue;
            }
            let dir = d / len_sq.sqrt();
            let center = dir * STAR_DIST;
            // Tangent basis around the star direction, with a random roll.
            let helper = if dir.y.abs() < 0.99 { Vec3::Y } else { Vec3::X };
            let right = helper.cross(dir).normalize();
            let up = dir.cross(right);
            let roll = rng.next_f32() * std::f32::consts::TAU;
            let (s, c) = roll.sin_cos();
            let r = right * c + up * s;
            let u = up * c - right * s;
            let (r, u) = (r * STAR_SIZE, u * STAR_SIZE);
            quads.push([center - r - u, center + r - u, center + r + u, center - r + u]);
        }
        quads
    }

    /// Build the per-frame celestial mesh (sun, moon, and — when visible — the
    /// stars), rotated for `time_ticks` and with the stars faded by
    /// `star_brightness`.
    pub fn build_mesh(
        time_ticks: f64,
        stars: &[[Vec3; 4]],
        star_brightness: f32,
    ) -> (Vec<Vertex>, Vec<u32>) {
        let rot = sky::celestial_rotation(time_ticks);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut push = |corners: [Vec3; 4], uvs: [[f32; 2]; 4], color: [f32; 4]| {
            let base = vertices.len() as u32;
            for (corner, uv) in corners.into_iter().zip(uvs) {
                vertices.push(Vertex {
                    position: rot.transform_point3(corner).into(),
                    color,
                    uv,
                    light: FULLBRIGHT,
                });
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        };

        // Sun: quad at +Y, corners (0,0)(1,0)(1,1)(0,1) of the sun sprite.
        let s = sky::SUN_SIZE;
        let d = sky::CELESTIAL_DIST;
        let [su0, sv0, su1, sv1] = sky_sun_rect();
        push(
            [
                Vec3::new(-s, d, -s),
                Vec3::new(s, d, -s),
                Vec3::new(s, d, s),
                Vec3::new(-s, d, s),
            ],
            [[su0, sv0], [su1, sv0], [su1, sv1], [su0, sv1]],
            [1.0, 1.0, 1.0, 1.0],
        );

        // Moon: quad at −Y, with the current phase tile (vanilla UV winding).
        let m = sky::MOON_SIZE;
        let [mu0, mv0, mu1, mv1] = sky_moon_rect(sky::moon_phase(time_ticks));
        push(
            [
                Vec3::new(-m, -d, m),
                Vec3::new(m, -d, m),
                Vec3::new(m, -d, -m),
                Vec3::new(-m, -d, -m),
            ],
            [[mu1, mv1], [mu0, mv1], [mu0, mv0], [mu1, mv0]],
            [1.0, 1.0, 1.0, 1.0],
        );

        // Stars: only when the night sky has faded them in.
        if star_brightness > 0.01 {
            let white = sky_white_uv();
            let color = [1.0, 1.0, 1.0, star_brightness];
            for quad in stars {
                push(*quad, [white; 4], color);
            }
        }

        (vertices, indices)
    }

    /// Tiny LCG so the star field is identical every run (no `rand` dep, and
    /// `Math.random` is unavailable in this codebase's constraints anyway).
    struct Lcg(u32);
    impl Lcg {
        fn new(seed: u32) -> Self {
            Self(seed | 1)
        }
        fn next_u32(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            self.0
        }
        fn next_f32(&mut self) -> f32 {
            (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
        }
        /// A value in [-1, 1).
        fn unit(&mut self) -> f32 {
            self.next_f32() * 2.0 - 1.0
        }
    }
}

fn create_depth_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> wgpu::TextureView {
    create_depth_view_sized(device, config.width, config.height)
}

fn create_depth_view_sized(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth-texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
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

/// Create the nametag text texture and a bind group matching the model pass's
/// group-1 (texture+sampler) layout.
fn create_nametag_resources(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::BindGroup) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("nametag-texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
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
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("nametag-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    (texture, bind_group)
}

/// Push one camera-facing quad (`top-left, top-right, bottom-right, bottom-left`)
/// into a `ModelVertex` billboard mesh.
#[allow(clippy::too_many_arguments)]
fn push_billboard(
    vertices: &mut Vec<ModelVertex>,
    indices: &mut Vec<u32>,
    center: Vec3,
    right: Vec3,
    up: Vec3,
    half_w: f32,
    half_h: f32,
    uvs: [[f32; 2]; 4],
    color: [f32; 4],
) {
    let base = vertices.len() as u32;
    let corners = [
        center - right * half_w + up * half_h,
        center + right * half_w + up * half_h,
        center + right * half_w - up * half_h,
        center - right * half_w - up * half_h,
    ];
    for (corner, uv) in corners.iter().zip(uvs) {
        vertices.push(ModelVertex {
            position: (*corner).into(),
            color,
            uv,
        });
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
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
    Vec<wgpu::BindGroup>,
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

    // Mipmaps: distant terrain otherwise samples the full-resolution atlas with
    // poor cache locality, which is murderous on bandwidth-starved iGPUs. The
    // chain is built per-tile (see build_atlas_mip_chain) so neighbours don't
    // bleed. Built before `atlas.pixels` is moved into `block_image` below.
    let mips = crate::texture::build_atlas_mip_chain(&atlas.pixels, atlas.width, atlas.height);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("block-atlas-texture"),
        size: wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, (data, mw, mh)) in mips.iter().enumerate() {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * mw),
                rows_per_image: Some(*mh),
            },
            wgpu::Extent3d {
                width: *mw,
                height: *mh,
                depth_or_array_layers: 1,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
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
    // One bind group per selectable mipmap level (vanilla "Mipmap Levels"):
    // index L caps the sampler to mips 0..=L via lod_max_clamp. L=0 reads only
    // the full-resolution mip 0 (nearest); higher L uses the trilinear chain,
    // sampling progressively smaller (cache-friendlier) mips at distance. In all
    // cases min_filter is Nearest so packed tiles never bleed within a level.
    let bind_groups: Vec<wgpu::BindGroup> = (0..crate::texture::ATLAS_MIP_LEVELS)
        .map(|level| {
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("block-atlas-sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::FilterMode::Linear,
                lod_max_clamp: level as f32,
                ..Default::default()
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("block-atlas-bind-group"),
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
            })
        })
        .collect();

    // Hand the CPU atlas image to the UI for block item thumbnails (reuses this
    // build instead of loading the atlas a second time).
    let block_image = image::RgbaImage::from_raw(atlas.width, atlas.height, atlas.pixels);
    (layout, bind_groups, biome_colors, atlas_uv, block_image)
}

fn create_panorama_resources(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    surface_format: wgpu::TextureFormat,
) -> Option<PanoramaResources> {
    let faces = crate::texture::load_panorama_faces()?;
    let face_w = faces[0].width();
    let face_h = faces[0].height();

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("panorama-array"),
        size: wgpu::Extent3d {
            width: face_w,
            height: face_h,
            depth_or_array_layers: 6,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    for (i, face) in faces.iter().enumerate() {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: i as u32 },
                aspect: wgpu::TextureAspect::All,
            },
            face,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * face_w),
                rows_per_image: Some(face_h),
            },
            wgpu::Extent3d {
                width: face_w,
                height: face_h,
                depth_or_array_layers: 1,
            },
        );
    }

    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("panorama-sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("panorama-uniform"),
        contents: bytemuck::bytes_of(&[0.0_f32; 4]),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("panorama-bind-group-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("panorama-bind-group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform_buffer.as_entire_binding(),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("panorama-pipeline-layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("panorama-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shader/panorama.wgsl").into()),
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("panorama-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        // The panorama draws inside the main pass (which has a depth attachment),
        // so the pipeline must declare a matching depth-stencil state. It's a
        // fullscreen backdrop: always pass, never write depth.
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

    log::info!("panorama loaded ({face_w}x{face_h}, 6 faces)");
    Some(PanoramaResources {
        pipeline,
        bind_group,
        uniform_buffer,
    })
}
