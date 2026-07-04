//! GPU resource builders & render helpers split out of the renderer: bind-group,
//! texture and depth-target creation, the dynamic-mesh uploader, the mod
//! post-effect type, and small per-frame helpers. Child module of `renderer`,
//! so `use super::*` pulls in the parent's private types and constants.
#![allow(clippy::too_many_arguments)]

use glam::Vec3;
use fpsmaster_core::SectionPos;
use wgpu::util::DeviceExt;
use crate::{
    texture::{EntityAtlasImage, SkyAtlasImage, TextureAtlasImage, TextureAtlasSource},
    ui::{UiBatch, UiGeometry, UiTextureId, UiVertex},
    AtlasUv, BiomeColors, Frustum, GuiAtlas, GuiTexture, ModelVertex, Vertex,
};
use super::*;

/// Upload an RGBA image as an sRGB UI atlas texture and build its bind group
/// (texture + the shared nearest sampler). The `BindGroup` keeps the texture and
/// view alive, so neither is returned.
fn upload_ui_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    data: &[u8],
    width: u32,
    height: u32,
    label: &str,
) -> wgpu::BindGroup {
    let (w, h) = (width.max(1), height.max(1));
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
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
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * w),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
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
    })
}

/// The atlas bind groups the batched UI pass samples: a shared 1×1 white texel
/// for solid fills, the font sheets, the GUI/item/block atlases and any
/// free-standing (favicon / mod) images. All are uploaded lazily on first use
/// (the raw-image cache is pruned each frame to what the frame references) and
/// bound through the shared `ui_bind_group_layout`.
pub(super) struct UiQuadResources {
    white: wgpu::BindGroup,
    ascii: wgpu::BindGroup,
    pages: HashMap<u8, wgpu::BindGroup>,
    gui: HashMap<GuiTexture, wgpu::BindGroup>,
    blocks: Option<wgpu::BindGroup>,
    items: Option<wgpu::BindGroup>,
    /// Keyed by `Arc` pointer identity so a stable favicon uploads once.
    raw: HashMap<usize, wgpu::BindGroup>,
}

impl UiQuadResources {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) -> Self {
        let white = upload_ui_texture(
            device,
            queue,
            layout,
            sampler,
            &[255, 255, 255, 255],
            1,
            1,
            "ui-white",
        );
        let ascii_img = crate::font::font().ascii_image();
        let (aw, ah) = ascii_img.dimensions();
        let ascii = upload_ui_texture(
            device,
            queue,
            layout,
            sampler,
            ascii_img.as_raw(),
            aw,
            ah,
            "ui-font-ascii",
        );
        Self {
            white,
            ascii,
            pages: HashMap::new(),
            gui: HashMap::new(),
            blocks: None,
            items: None,
            raw: HashMap::new(),
        }
    }

    /// Ensure every texture referenced by `geos` this frame is uploaded, and drop
    /// cached raw images the frame no longer references. (Uploads are gated on the
    /// source image actually loading, so `contains_key` + `insert` is clearer than
    /// the entry API here.)
    #[allow(clippy::map_entry)]
    pub(super) fn ensure(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        gui: &GuiAtlas,
        geos: &[&UiGeometry],
    ) {
        let mut needed_raw: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for geo in geos {
            for img in &geo.raw_images {
                needed_raw.insert(std::sync::Arc::as_ptr(img) as usize);
            }
        }
        self.raw.retain(|key, _| needed_raw.contains(key));

        for geo in geos {
            for batch in &geo.batches {
                match batch.texture {
                    UiTextureId::White | UiTextureId::FontAscii => {}
                    UiTextureId::FontPage(page) => {
                        if !self.pages.contains_key(&page) {
                            if let Some(img) = crate::font::font().page_image(page) {
                                let (w, h) = img.dimensions();
                                let bg = upload_ui_texture(
                                    device,
                                    queue,
                                    layout,
                                    sampler,
                                    img.as_raw(),
                                    w,
                                    h,
                                    "ui-font-page",
                                );
                                self.pages.insert(page, bg);
                            }
                        }
                    }
                    UiTextureId::Gui(texture) => {
                        if !self.gui.contains_key(&texture) {
                            if let Some(img) = gui.image(texture) {
                                let (w, h) = img.dimensions();
                                let bg = upload_ui_texture(
                                    device, queue, layout, sampler, img.as_raw(), w, h, "ui-gui",
                                );
                                self.gui.insert(texture, bg);
                            }
                        }
                    }
                    UiTextureId::Blocks => {
                        if self.blocks.is_none() {
                            if let Some(img) = gui.blocks_image() {
                                let (w, h) = img.dimensions();
                                self.blocks = Some(upload_ui_texture(
                                    device, queue, layout, sampler, img.as_raw(), w, h, "ui-blocks",
                                ));
                            }
                        }
                    }
                    UiTextureId::Items => {
                        if self.items.is_none() {
                            if let Some(img) = gui.items_image() {
                                let (w, h) = img.dimensions();
                                self.items = Some(upload_ui_texture(
                                    device, queue, layout, sampler, img.as_raw(), w, h, "ui-items",
                                ));
                            }
                        }
                    }
                    UiTextureId::Raw(idx) => {
                        if let Some(img) = geo.raw_images.get(idx) {
                            let key = std::sync::Arc::as_ptr(img) as usize;
                            if !self.raw.contains_key(&key) {
                                let (w, h) = img.dimensions();
                                let bg = upload_ui_texture(
                                    device, queue, layout, sampler, img.as_raw(), w, h, "ui-raw",
                                );
                                self.raw.insert(key, bg);
                            }
                        }
                    }
                }
            }
        }
    }

    /// The bind group for a batch's texture, or None if it failed to load (the
    /// batch is then skipped — a missing texture draws nothing, as before).
    pub(super) fn bind_group(
        &self,
        texture: UiTextureId,
        geo: &UiGeometry,
    ) -> Option<&wgpu::BindGroup> {
        match texture {
            UiTextureId::White => Some(&self.white),
            UiTextureId::FontAscii => Some(&self.ascii),
            UiTextureId::FontPage(page) => self.pages.get(&page),
            UiTextureId::Gui(texture) => self.gui.get(&texture),
            UiTextureId::Blocks => self.blocks.as_ref(),
            UiTextureId::Items => self.items.as_ref(),
            UiTextureId::Raw(idx) => {
                let img = geo.raw_images.get(idx)?;
                self.raw.get(&(std::sync::Arc::as_ptr(img) as usize))
            }
        }
    }
}

/// One tessellated UI layer's vertex buffer plus its batch list. The buffer grows
/// as needed and is rewritten each frame; empty layers keep the buffer and just
/// stop drawing.
pub(super) struct UiQuadLayer {
    pub(super) buffer: Option<wgpu::Buffer>,
    capacity_verts: u64,
    pub(super) geo: UiGeometry,
    label: &'static str,
}

impl UiQuadLayer {
    pub(super) fn new(label: &'static str) -> Self {
        Self {
            buffer: None,
            capacity_verts: 0,
            geo: UiGeometry::default(),
            label,
        }
    }

    /// Replace this layer's geometry, growing (with headroom) and rewriting the
    /// vertex buffer. Takes `geo` by value so its batches / raw-image keepalives
    /// stay owned for the frame's draws.
    pub(super) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, geo: UiGeometry) {
        if !geo.vertices.is_empty() {
            let needed = geo.vertices.len() as u64;
            if self.buffer.is_none() || needed > self.capacity_verts {
                let capacity = needed.next_power_of_two().max(256);
                self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(self.label),
                    size: capacity * std::mem::size_of::<UiVertex>() as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.capacity_verts = capacity;
            }
            queue.write_buffer(
                self.buffer.as_ref().expect("ui layer buffer just set"),
                0,
                bytemuck::cast_slice(&geo.vertices),
            );
        }
        self.geo = geo;
    }

    pub(super) fn batches(&self) -> &[UiBatch] {
        &self.geo.batches
    }
}

/// Refill a persistent vertex+index buffer pair in place, reallocating (with
/// headroom) only when the geometry outgrows the current capacity. An
/// `index_count` of 0 keeps the buffers and just stops drawing.
#[allow(clippy::too_many_arguments)]
pub(super) fn fill_dynamic_mesh(
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

/// Geometry for the mining crack overlay: the `destroy_stage_<stage>` atlas tile
/// applied to every face of the mined block's render boxes, each slightly
/// inflated so the overlay never z-fights the block. A partial block cracks only
/// over its shape (a slab over its half, stairs over their steps) — the texture
/// crops to each box exactly like the world mesher, so the crack lines up with
/// the visible faces instead of floating on a full cube. Drawn with the
/// translucent pipeline, so the crack texels alpha-blend over the block beneath
/// (vanilla look). Blocks with no render boxes (cross/torch/…) fall back to a
/// full cube so the crack stays visible.
pub(super) fn break_overlay_geometry(
    x: i32,
    y: i32,
    z: i32,
    stage: u8,
    block: fpsmaster_core::BlockState,
    atlas: &AtlasUv,
) -> (Vec<Vertex>, Vec<u32>) {
    let rect = atlas.tile_rect(Some(&format!("destroy_stage_{}", stage.min(9))));
    let computed = block.render_boxes();
    let fallback = [fpsmaster_core::BlockBox { min: [0.0; 3], max: [1.0; 3] }];
    let boxes: &[fpsmaster_core::BlockBox] = if computed.as_slice().is_empty() {
        &fallback
    } else {
        computed.as_slice()
    };

    // Inflate past the block faces so the overlay never z-fights them.
    const PAD: f32 = 0.004;

    // Each face: outward normal (for the UV convention) plus four corners, where
    // a corner component selects the box min (0) or max (1) along that axis. The
    // translucent pipeline doesn't cull, so the winding only needs to be
    // consistent.
    const FACES: [([i32; 3], [[u8; 3]; 4]); 6] = [
        ([1, 0, 0], [[1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1]]),
        ([-1, 0, 0], [[0, 0, 1], [0, 1, 1], [0, 1, 0], [0, 0, 0]]),
        ([0, 1, 0], [[0, 1, 1], [1, 1, 1], [1, 1, 0], [0, 1, 0]]),
        ([0, -1, 0], [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]]),
        ([0, 0, 1], [[1, 0, 1], [1, 1, 1], [0, 1, 1], [0, 0, 1]]),
        ([0, 0, -1], [[0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0]]),
    ];

    let mut vertices = Vec::with_capacity(24 * boxes.len());
    let mut indices = Vec::with_capacity(36 * boxes.len());
    for b in boxes {
        let bmin = [b.min[0] as f32, b.min[1] as f32, b.min[2] as f32];
        let bmax = [b.max[0] as f32, b.max[1] as f32, b.max[2] as f32];
        for (normal, corners) in FACES {
            let base = vertices.len() as u32;
            for c in corners {
                // Box-local coord (0..1 within the cell) drives the UV so the
                // crack crops to the box; PAD inflates only the world position.
                let lc = [
                    if c[0] == 0 { bmin[0] } else { bmax[0] },
                    if c[1] == 0 { bmin[1] } else { bmax[1] },
                    if c[2] == 0 { bmin[2] } else { bmax[2] },
                ];
                let pos = [
                    x as f32 + lc[0] + if c[0] == 0 { -PAD } else { PAD },
                    y as f32 + lc[1] + if c[1] == 0 { -PAD } else { PAD },
                    z as f32 + lc[2] + if c[2] == 0 { -PAD } else { PAD },
                ];
                let (fu, fv) = crate::gui_item::face_uv(normal, lc[0], lc[1], lc[2]);
                vertices.push(Vertex {
                    position: pos,
                    color: [1.0, 1.0, 1.0, 1.0],
                    uv: [rect[0] + fu * rect[2], rect[1] + fv * rect[3]],
                    light: crate::FULLBRIGHT,
                });
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    (vertices, indices)
}

/// Round `needed` bytes up to a buffer capacity with ~1.5x headroom, 4-byte
/// aligned and at least 4 bytes, so a dynamic mesh hovering around a size does
/// not reallocate every frame (`queue.write_buffer` needs a 4-aligned range).
pub(super) fn grow_capacity(needed: u64) -> u64 {
    let with_headroom = needed.saturating_add(needed / 2);
    let aligned = (with_headroom + 3) & !3;
    aligned.max(4)
}

/// Whether any part of a 16×16×16 section is inside the view frustum. Unlike the
/// old column test this also culls vertically, so sections under the floor or
/// high above are skipped.
pub(super) fn section_in_frustum(frustum: &Frustum, pos: SectionPos) -> bool {
    let min = Vec3::new((pos.x * 16) as f32, (pos.y * 16) as f32, (pos.z * 16) as f32);
    let max = min + Vec3::splat(16.0);
    frustum.intersects_aabb(min, max)
}

/// Choose a present mode for the requested vsync state. `Fifo` is guaranteed to
/// be supported and gives true vertical sync; when vsync is disabled we prefer
/// `Mailbox` (low latency, no tearing) and fall back to `Immediate` then `Fifo`.
pub(super) fn pick_present_mode(present_modes: &[wgpu::PresentMode], vsync: bool) -> wgpu::PresentMode {
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

/// How many frames the surface may keep in flight, chosen per present mode.
///
/// Fifo (vsync) wants 2: a queued frame absorbs an occasional frame-time spike so
/// the vsync cadence doesn't hitch. The unsynced modes want 1: with a single
/// in-flight frame the drawable back-pressure paces presentation to roughly the
/// display rate (the render loop blocks acquiring the next drawable), which stops
/// Immediate from scanning out multiple half-drawn frames per refresh — i.e. the
/// tearing that reads as GUI "flicker". It also gives the lowest input latency,
/// which is the whole point of running vsync off.
pub(super) fn frame_latency_for(mode: wgpu::PresentMode) -> u32 {
    match mode {
        wgpu::PresentMode::Fifo | wgpu::PresentMode::FifoRelaxed => 2,
        _ => 1,
    }
}

// Depth32Float (not Depth24Plus) so the post pass can sample the world depth for
// depth-of-field and motion blur (Depth24Plus is not reliably loadable).
pub(super) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Linear HDR format for the off-screen world target, so the post pass can
/// tone-map highlights (sun/specular/bloom) above 1.0 instead of clipping.
pub(super) const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Sun shadow map resolution and format (quality-first; cheap on a dGPU).
pub(super) const SHADOW_DIM: u32 = 1024;
pub(super) const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// World-space half-extent of the shadow frustum centred near the camera.
pub(super) const SHADOW_RADIUS: f32 = 96.0;

/// A single-binding bind-group layout holding one uniform buffer.
pub(super) fn create_uniform_layout(
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
pub(super) fn create_sky_atlas_bind_group(
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
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas.pixels,
        wgpu::TexelCopyBufferLayout {
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
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
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

/// Upload `particles.png` (the 16×16 particle sprite sheet) and build a
/// texture+sampler bind group the particle draw samples at group 1. Reuses the
/// block-atlas layout so the overlay pipeline accepts it. Falls back to a single
/// transparent texel if the asset is missing, so particles simply don't show
/// rather than crashing. Nearest filtering keeps the pixel sprites crisp.
pub(super) fn create_particle_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::BindGroup {
    create_asset_texture_bind_group(
        device,
        queue,
        layout,
        "assets/minecraft/textures/particle/particles.png",
        "particle",
    )
}

/// Upload a single asset PNG and build a nearest-filtered texture+sampler bind
/// group on the block-atlas layout (so the overlay pipeline can sample it).
/// Falls back to a transparent texel if the asset is missing, so the draw is
/// simply invisible rather than crashing. Used for the particle sheet and the
/// experience-orb sheet.
pub(super) fn create_asset_texture_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    asset: &str,
    label: &str,
) -> wgpu::BindGroup {
    let image = crate::texture::load_asset_image(asset);
    let (width, height, pixels) = match image {
        Some(img) => (img.width(), img.height(), img.into_raw()),
        None => {
            log::warn!("{asset} not found; {label} will be invisible");
            (1, 1, vec![0u8, 0, 0, 0])
        }
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&format!("{label}-texture")),
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
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
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
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(&format!("{label}-sampler")),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&format!("{label}-bind-group")),
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

/// Upload `enchanted_item_glint.png` and build a REPEAT-wrapped texture+sampler
/// bind group on the block-atlas layout (so the glint pipeline can sample it at
/// group 3). The glint UV is scaled past 1.0 and scrolled, so the texture must
/// tile; linear filtering keeps the streaks smooth as they travel. Falls back to
/// a single transparent texel if the asset is missing, so the glint is simply
/// absent rather than crashing.
pub(super) fn create_glint_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::BindGroup {
    let image = crate::texture::load_asset_image(
        "assets/minecraft/textures/misc/enchanted_item_glint.png",
    );
    let (width, height, pixels) = match image {
        Some(img) => (img.width(), img.height(), img.into_raw()),
        None => {
            log::warn!("enchanted_item_glint.png not found; the glint will be absent");
            (1, 1, vec![0u8, 0, 0, 0])
        }
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glint-texture"),
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
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
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
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("glint-sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("glint-bind-group"),
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
pub(super) mod sky_geometry {
    use crate::sky;
    use crate::{Vertex, FULLBRIGHT};
    use crate::texture::{sky_moon_rect, sky_sun_rect, sky_white_uv};
    use glam::Vec3;

    const STAR_COUNT: usize = 1500;
    const STAR_DIST: f32 = 100.0;
    const STAR_SIZE: f32 = 0.35;

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

pub(super) fn create_depth_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> wgpu::TextureView {
    create_depth_view_sized(device, config.width, config.height).1
}

pub(super) fn create_depth_view_sized(device: &wgpu::Device, width: u32, height: u32) -> (wgpu::Texture, wgpu::TextureView) {
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
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Upload the entity texture atlas (player skin + white texel) and build the
/// texture+sampler bind group the model pass samples at group 1.
/// A compiled mod full-screen post effect (see [`Renderer::set_post_effect`]).
pub(super) struct PostEffect {
    pub(super) pipeline: wgpu::RenderPipeline,
    pub(super) layout: wgpu::BindGroupLayout,
    pub(super) sampler: wgpu::Sampler,
    pub(super) uniform_buf: wgpu::Buffer,
    /// Samples `post_fx_tex` (+ sampler + uniforms); rebuilt on resize.
    pub(super) bind_group: Option<wgpu::BindGroup>,
}

/// Fixed WGSL prepended to a mod post-effect snippet: a full-screen-triangle
/// vertex stage, the scene-color binding, a `U` uniform (`resolution`, `time`),
/// and an `fs_main` that calls the mod's `effect(uv, color)`.
pub(super) const POST_EFFECT_PRELUDE: &str = r#"
struct Uniforms { resolution: vec2<f32>, time: f32, _pad: f32, };
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;
@group(0) @binding(2) var<uniform> U: Uniforms;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32>, };

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    let xy = p[vi];
    var o: VsOut;
    o.pos = vec4<f32>(xy, 0.0, 1.0);
    o.uv = vec2<f32>(xy.x * 0.5 + 0.5, 1.0 - (xy.y * 0.5 + 0.5));
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let color = textureSample(src_tex, src_samp, in.uv);
    return effect(in.uv, color);
}
"#;

/// Upload an RGBA8 image and build a `(texture, sampler)` bind group against an
/// existing layout (used for mod-supplied native-geometry textures). Nearest
/// filtering + clamp, matching the entity atlas.
#[allow(clippy::too_many_arguments)]
pub(super) fn create_rgba_texture_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    rgba: &[u8],
    width: u32,
    height: u32,
    label: &str,
) -> wgpu::BindGroup {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),
            rows_per_image: Some(height),
        },
        size,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
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

pub(super) fn create_entity_texture_bind_group(
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
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas.pixels,
        wgpu::TexelCopyBufferLayout {
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
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
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
pub(super) fn create_nametag_resources(
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
pub(super) fn push_billboard(
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

pub(super) fn create_ui_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
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

pub(super) fn create_texture_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (
    wgpu::BindGroupLayout,
    Vec<wgpu::BindGroup>,
    BiomeColors,
    AtlasUv,
    Option<image::RgbaImage>,
    wgpu::Texture,
    Vec<crate::texture::AnimatedTile>,
) {
    let mut atlas = TextureAtlasImage::load_default();
    let animated = std::mem::take(&mut atlas.animated);
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
        TextureAtlasSource::ResourcePack(path) => {
            log::info!(
                "loaded block atlas from resource pack {}",
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
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
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
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
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
    (layout, bind_groups, biome_colors, atlas_uv, block_image, texture, animated)
}

pub(super) fn create_default_pbr_textures(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::TextureView, wgpu::TextureView, wgpu::Sampler) {
    let make_1x1 = |label: &str, pixel: [u8; 4]| {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixel,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    };
    // Flat normal pointing up: (128,128,255) = (0,0,1) in tangent space; A=255 (no AO).
    let normal_view = make_1x1("pbr-default-normal", [128, 128, 255, 255]);
    // Zero specular: rough, non-metallic, no emissive.
    let specular_view = make_1x1("pbr-default-specular", [0, 0, 0, 255]);
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("pbr-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (normal_view, specular_view, sampler)
}

pub(super) fn create_lighting_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    lighting_buffer: &wgpu::Buffer,
    shadow_view: &wgpu::TextureView,
    shadow_compare_sampler: &wgpu::Sampler,
    pbr_normal_view: &wgpu::TextureView,
    pbr_specular_view: &wgpu::TextureView,
    pbr_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lighting-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: lighting_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(shadow_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(shadow_compare_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(pbr_normal_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(pbr_specular_view),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(pbr_sampler),
            },
        ],
    })
}

pub(super) fn create_panorama_resources(
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
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: i as u32 },
                aspect: wgpu::TextureAspect::All,
            },
            face,
            wgpu::TexelCopyBufferLayout {
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
        bind_group_layouts: &[Some(&bind_group_layout)],
    immediate_size: 0,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("panorama-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shader/panorama.wgsl").into()),
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("panorama-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
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
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
    cache: None,
    multiview_mask: None,
    });

    log::info!("panorama loaded ({face_w}x{face_h}, 6 faces)");
    Some(PanoramaResources {
        pipeline,
        bind_group,
        uniform_buffer,
    })
}

