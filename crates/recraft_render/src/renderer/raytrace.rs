//! Hardware ray tracing for sun shadows + ambient occlusion.
//!
//! Builds one bottom-level acceleration structure (BLAS) per chunk section from its
//! opaque (solid + cutout) geometry, and a single top-level acceleration structure
//! (TLAS) instancing the in-range sections each frame. The chunk fragment shader
//! reads the TLAS through group(3) (see `shader/rt_common.wgsl`) and casts rays for
//! sharp/soft sun shadows and RTAO.
//!
//! Only constructed when the adapter exposes `Features::EXPERIMENTAL_RAY_QUERY` and
//! the user enabled ray tracing — the default build never touches this module's GPU
//! resources. Positions are stored **section-local** (vertex world pos minus the
//! section origin) as `Float32x3`; the per-frame TLAS instance transform translates
//! each section into camera-relative space (`section_origin - camera_origin`), the
//! same frame the chunk shader's `in.world_pos` lives in, so ray origins line up and
//! float precision stays high far from the world origin.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::IVec3;
use recraft_core::SectionPos;

use crate::{ChunkMeshBuffers, ChunkVertex, EmissiveLight};

/// Max emissive point lights uploaded to the shader per frame (the nearest in-range
/// emitters). The chunk RT shader loops over all of them, distance-culling cheaply.
const MAX_LIGHTS: usize = 128;

/// GPU layout of one ray-traced point light (group 3 binding 3, std430 array).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct GpuLight {
    /// xyz: camera-relative position (blocks); w: radius (blocks).
    pos_radius: [f32; 4],
    /// rgb: light colour; w: intensity (0 = empty slot).
    color: [f32; 4],
}

/// Sections farther than this (blocks, 3D) from the camera are left out of the TLAS,
/// and sun-shadow rays are capped at the same length. ~10 chunks: enough for terrain
/// sun shadows while bounding the per-frame TLAS build to a few thousand instances.
/// (RTAO rays use a much shorter, separate contact radius set in `RtParams` — AO is a
/// local effect, so a long AO ray would wrongly darken everything near any geometry.)
const RT_RANGE_BLOCKS: f32 = 160.0;

/// Parameters fed to the chunk shader (group 3 binding 1). Mirrors `RtParams` in
/// `shader/rt_common.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct RtParams {
    /// x: shadow ray tmax (blocks), y: AO ray tmax (blocks), z: frame counter,
    /// w: sun angular radius (radians, soft-shadow penumbra).
    pub config: [f32; 4],
    /// x: shadow samples, y: AO samples, z: AO strength, w: unused.
    pub quality: [f32; 4],
}

/// A BLAS + its per-triangle colours and pool base. Used for a section's stained-glass
/// geometry (a separate BLAS so it can be ray-masked independently of the solid one).
struct RtGeom {
    blas: wgpu::Blas,
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    size: wgpu::BlasTriangleGeometrySizeDescriptor,
    tri_colors: Vec<u32>,
    tri_normals: Vec<u32>,
    tri_uvs: Vec<u32>,
    tri_positions: Vec<f32>,
    tri_base: u32,
}

/// The geometry pieces produced by `build_blas_geom` before a pool base is assigned.
struct BlasGeom {
    blas: wgpu::Blas,
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    size: wgpu::BlasTriangleGeometrySizeDescriptor,
    tri_colors: Vec<u32>,
    tri_normals: Vec<u32>,
    tri_uvs: Vec<u32>,
    tri_positions: Vec<f32>,
}

/// One section's ray-tracing geometry. A section is kept if it has solid/cutout geometry
/// (the opaque BLAS, ray-masked 0x01) OR stained glass (a separate BLAS, ray-masked 0x02)
/// — the latter alone matters for a glass box around a light, whose walls may live in a
/// section with no opaque geometry of their own.
struct RtSection {
    /// Section world origin in blocks; the TLAS instance translates by this minus
    /// the camera origin each frame.
    origin: IVec3,
    /// Opaque (solid + cutout) geometry. None for a glass-only section.
    solid: Option<RtGeom>,
    /// Stained-glass geometry (ray-masked 0x02): point-light shadow rays sample its
    /// colour to tint light, without it occluding sun/GI/reflections.
    glass: Option<RtGeom>,
    /// Water geometry (ray-masked 0x04): only the path tracer traces it (Fresnel
    /// reflect/refract); the raster path draws water itself.
    water: Option<RtGeom>,
    /// Emissive blocks in this section (section-local), collected into the per-frame
    /// point-light buffer when in range.
    lights: Vec<EmissiveLight>,
}

/// Capacity (in triangles / u32s) of the shared per-triangle colour pool. 16M is the
/// 24-bit instance-custom-data ceiling; well above the in-range triangle count.
const TRI_POOL_CAPACITY: u32 = 1 << 22; // 4M triangles (the position pool is 9 floats each)

pub struct RayTracer {
    sections: HashMap<SectionPos, RtSection>,
    /// Sections whose BLAS still needs building (newly uploaded since the last build).
    dirty: Vec<SectionPos>,
    tlas: wgpu::Tlas,
    max_instances: u32,
    /// Instances written last frame, so the unused tail can be cleared to None.
    prev_instance_count: u32,
    params_buffer: wgpu::Buffer,
    /// Shared per-triangle colour pool (storage buffer, one packed RGB per triangle of
    /// every section), bump-allocated; re-packed compactly when it would overflow.
    tri_color_pool: wgpu::Buffer,
    /// Parallel per-triangle packed-normal pool (same index as `tri_color_pool`), for
    /// the path tracer to shade + bounce off hits.
    tri_normal_pool: wgpu::Buffer,
    /// Per-triangle atlas UVs (3 packed u32 per triangle, indexed `tri_index * 3`), for
    /// the path tracer to sample the real texture at a hit.
    tri_uv_pool: wgpu::Buffer,
    /// Per-triangle vertex positions (9 f32 per triangle, indexed `tri_index * 9`), for
    /// the path tracer to compute the hit barycentrics manually.
    tri_pos_pool: wgpu::Buffer,
    tri_top: u32,
    /// Per-frame point-light buffer (MAX_LIGHTS slots; unused slots have intensity 0).
    light_buffer: wgpu::Buffer,
    /// Atlas pixels, sampled per triangle so the GI bounce colour is the real surface
    /// colour (texture × tint), not just the biome tint.
    atlas: Option<std::sync::Arc<image::RgbaImage>>,
    bind_group: wgpu::BindGroup,
}

impl RayTracer {
    /// The group(3) layout the chunk RT pipelines and `RayTracer::new` share:
    /// binding 0 = TLAS, binding 1 = `RtParams` uniform.
    pub fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rt-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::AccelerationStructure {
                        vertex_return: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 2: the per-triangle colour pool (read-only storage). Only the
                // RT AO/GI pass reads it; the chunk RT pipelines that share this layout
                // simply don't reference it.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 3: emissive point lights (read-only storage), read by the
                // chunk RT shader for ray-traced point lighting.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 4: per-triangle packed normals (parallel to the colour pool,
                // same index). Read by the path tracer to shade + bounce off hits.
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 5: per-triangle atlas UVs (3 packed u32 per triangle), so the
                // path tracer can barycentric-interpolate the hit UV and sample the real
                // texture (not just the per-triangle average colour).
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 6: per-triangle vertex positions (9 f32 = 3 section-local verts),
                // so the path tracer can compute the hit barycentrics itself (naga's ray
                // intersection barycentrics aren't reliably populated).
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        })
    }

    pub fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        max_instances: u32,
        atlas: Option<std::sync::Arc<image::RgbaImage>>,
    ) -> Self {
        let tlas = device.create_tlas(&wgpu::CreateTlasDescriptor {
            label: Some("rt-tlas"),
            max_instances,
            flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
            update_mode: wgpu::AccelerationStructureUpdateMode::Build,
        });
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt-params"),
            size: std::mem::size_of::<RtParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tri_color_pool = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt-tri-color-pool"),
            size: TRI_POOL_CAPACITY as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt-light-buffer"),
            size: (MAX_LIGHTS * std::mem::size_of::<GpuLight>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tri_normal_pool = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt-tri-normal-pool"),
            size: TRI_POOL_CAPACITY as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tri_uv_pool = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt-tri-uv-pool"),
            size: TRI_POOL_CAPACITY as u64 * 3 * 4, // 3 packed UVs per triangle
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tri_pos_pool = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt-tri-pos-pool"),
            size: TRI_POOL_CAPACITY as u64 * 9 * 4, // 3 vertex positions (9 f32) per triangle
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rt-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: tlas.as_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: tri_color_pool.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: light_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: tri_normal_pool.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: tri_uv_pool.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: tri_pos_pool.as_entire_binding(),
                },
            ],
        });
        Self {
            sections: HashMap::new(),
            dirty: Vec::new(),
            tlas,
            max_instances,
            prev_instance_count: 0,
            params_buffer,
            tri_color_pool,
            tri_normal_pool,
            tri_uv_pool,
            tri_pos_pool,
            tri_top: 0,
            light_buffer,
            atlas,
            bind_group,
        }
    }

    /// Re-pack the per-triangle colour pool compactly (drops dead ranges left by removed
    /// / re-meshed sections). Called when a fresh allocation would overflow the pool.
    /// Each section's `tri_base` is updated; the next `build()` re-stamps the instances.
    fn repack(&mut self, queue: &wgpu::Queue) {
        let mut top = 0u32;
        for sec in self.sections.values_mut() {
            if let Some(s) = sec.solid.as_mut() {
                s.tri_base = top;
                queue.write_buffer(&self.tri_color_pool, top as u64 * 4, bytemuck::cast_slice(&s.tri_colors));
                queue.write_buffer(&self.tri_normal_pool, top as u64 * 4, bytemuck::cast_slice(&s.tri_normals));
                queue.write_buffer(&self.tri_uv_pool, top as u64 * 3 * 4, bytemuck::cast_slice(&s.tri_uvs));
                queue.write_buffer(&self.tri_pos_pool, top as u64 * 9 * 4, bytemuck::cast_slice(&s.tri_positions));
                top += s.tri_colors.len() as u32;
            }
            if let Some(g) = sec.glass.as_mut() {
                g.tri_base = top;
                queue.write_buffer(&self.tri_color_pool, top as u64 * 4, bytemuck::cast_slice(&g.tri_colors));
                queue.write_buffer(&self.tri_normal_pool, top as u64 * 4, bytemuck::cast_slice(&g.tri_normals));
                queue.write_buffer(&self.tri_uv_pool, top as u64 * 3 * 4, bytemuck::cast_slice(&g.tri_uvs));
                queue.write_buffer(&self.tri_pos_pool, top as u64 * 9 * 4, bytemuck::cast_slice(&g.tri_positions));
                top += g.tri_colors.len() as u32;
            }
            if let Some(w) = sec.water.as_mut() {
                w.tri_base = top;
                queue.write_buffer(&self.tri_color_pool, top as u64 * 4, bytemuck::cast_slice(&w.tri_colors));
                queue.write_buffer(&self.tri_normal_pool, top as u64 * 4, bytemuck::cast_slice(&w.tri_normals));
                queue.write_buffer(&self.tri_uv_pool, top as u64 * 3 * 4, bytemuck::cast_slice(&w.tri_uvs));
                queue.write_buffer(&self.tri_pos_pool, top as u64 * 9 * 4, bytemuck::cast_slice(&w.tri_positions));
                top += w.tri_colors.len() as u32;
            }
        }
        self.tri_top = top;
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub fn params_buffer(&self) -> &wgpu::Buffer {
        &self.params_buffer
    }

    /// (Re)build a section's BLAS geometry from its opaque mesh layers. Solid +
    /// cutout cast shadows / occlude; transparent + water do not. Called from
    /// `upload_chunk_mesh`, so it tracks the section mesh lifecycle exactly.
    pub fn upload_section(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pos: SectionPos,
        solid: &ChunkMeshBuffers,
        cutout: &ChunkMeshBuffers,
        transparent: &ChunkMeshBuffers,
        water: &ChunkMeshBuffers,
        lights: &[EmissiveLight],
    ) {
        let origin = IVec3::new(pos.x * 16, pos.y * 16, pos.z * 16);
        // Opaque (solid + cutout, mask 0x01), stained glass (0x02) and water (0x04) each
        // get their own BLAS so the path tracer can mask + handle them differently. Keep
        // the section if it has ANY of them.
        let solid_geom = self.build_blas_geom(device, queue, origin, &[solid, cutout], "rt-section", 0);
        let glass_geom = self.build_blas_geom(device, queue, origin, &[transparent], "rt-glass", 1);
        let water_geom = self.build_blas_geom(device, queue, origin, &[water], "rt-water", 2);
        if solid_geom.is_none() && glass_geom.is_none() && water_geom.is_none() {
            self.remove_section(pos);
            return;
        }

        // One combined pool allocation (repack-safe): [solid, glass, water] colours.
        let solid_n = solid_geom.as_ref().map_or(0, |g| g.tri_colors.len() as u32);
        let glass_n = glass_geom.as_ref().map_or(0, |g| g.tri_colors.len() as u32);
        let water_n = water_geom.as_ref().map_or(0, |g| g.tri_colors.len() as u32);
        if self.tri_top + solid_n + glass_n + water_n > TRI_POOL_CAPACITY {
            self.repack(queue);
        }
        let base = self.tri_top;
        self.tri_top += solid_n + glass_n + water_n;
        let solid = solid_geom.map(|g| self.store_geom(queue, g, base));
        let glass = glass_geom.map(|g| self.store_geom(queue, g, base + solid_n));
        let water = water_geom.map(|g| self.store_geom(queue, g, base + solid_n + glass_n));

        self.sections.insert(
            pos,
            RtSection {
                origin,
                solid,
                glass,
                water,
                lights: lights.to_vec(),
            },
        );
        self.dirty.push(pos);
    }

    /// Write a built BLAS geometry's colours + normals into the pool at `base` and wrap it
    /// as an `RtGeom`.
    fn store_geom(&self, queue: &wgpu::Queue, g: BlasGeom, base: u32) -> RtGeom {
        if !g.tri_colors.is_empty() {
            queue.write_buffer(&self.tri_color_pool, base as u64 * 4, bytemuck::cast_slice(&g.tri_colors));
            queue.write_buffer(&self.tri_normal_pool, base as u64 * 4, bytemuck::cast_slice(&g.tri_normals));
            // UV pool: 3 u32 per triangle → offset base*3. Position pool: 9 f32 → base*9.
            queue.write_buffer(&self.tri_uv_pool, base as u64 * 3 * 4, bytemuck::cast_slice(&g.tri_uvs));
            queue.write_buffer(&self.tri_pos_pool, base as u64 * 9 * 4, bytemuck::cast_slice(&g.tri_positions));
        }
        RtGeom {
            blas: g.blas,
            vertex_buf: g.vertex_buf,
            index_buf: g.index_buf,
            size: g.size,
            tri_colors: g.tri_colors,
            tri_normals: g.tri_normals,
            tri_uvs: g.tri_uvs,
            tri_positions: g.tri_positions,
            tri_base: base,
        }
    }

    /// Build a BLAS + its per-triangle colours from one or more mesh buffers (their
    /// indices concatenated). None if there's no geometry. The colours are not yet
    /// pool-allocated — the caller assigns a `tri_base`.
    fn build_blas_geom(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        origin: IVec3,
        bufs: &[&ChunkMeshBuffers],
        label: &str,
        // 0 = solid/cutout, 1 = glass, 2 = water. Packed into the tri-colour flags so the
        // path tracer knows how to handle a hit (alpha test / tint pass-through / Fresnel).
        material: u32,
    ) -> Option<BlasGeom> {
        let vertex_count: usize = bufs.iter().map(|b| b.vertices.len()).sum();
        let index_count: usize = bufs.iter().map(|b| b.indices.len()).sum();
        if index_count == 0 {
            return None;
        }
        let (ox, oy, oz) = (origin.x as f32, origin.y as f32, origin.z as f32);
        // Section-local float positions (decode the fixed-point ×64 i32 to blocks, then
        // make section-relative so magnitudes stay in ~[0, 16]).
        let to_local = |v: &ChunkVertex| -> [f32; 3] {
            [
                v.pos_light[0] as f32 / 64.0 - ox,
                v.pos_light[1] as f32 / 64.0 - oy,
                v.pos_light[2] as f32 / 64.0 - oz,
            ]
        };
        let mut positions: Vec<[f32; 3]> = Vec::with_capacity(vertex_count);
        let mut indices: Vec<u32> = Vec::with_capacity(index_count);
        let mut vbase = 0u32;
        for b in bufs {
            positions.extend(b.vertices.iter().map(&to_local));
            indices.extend(b.indices.iter().map(|&i| i as u32 + vbase));
            vbase += b.vertices.len() as u32;
        }
        // Flat vertex view across the buffers, for centroid colour sampling.
        let verts: Vec<&ChunkVertex> = bufs.iter().flat_map(|b| b.vertices.iter()).collect();

        // Per-triangle surface colour (8:8:8 RGB) in BLAS primitive order — fetched at a
        // ray hit via tri_color_pool[instance_custom_data + primitive_index]. The colour is
        // the atlas texel at the triangle's centroid UV times the vertex tint.
        let tri_count_usize = indices.len() / 3;
        let atlas = self.atlas.as_ref();
        // bit 25 = glass, bit 26 = water (constant per BLAS).
        let mat_bits = (((material == 1) as u32) << 25) | (((material == 2) as u32) << 26);
        let tri_colors: Vec<u32> = (0..tri_count_usize)
            .map(|t| {
                let v0 = verts[indices[t * 3] as usize];
                let tint = v0.color;
                let v1 = verts[indices[t * 3 + 1] as usize];
                let v2 = verts[indices[t * 3 + 2] as usize];
                // (colour, coverage 0..31): opaque centroid → full coverage; a cutout hole
                // → the alpha-weighted average colour + the average alpha as coverage, so
                // the path tracer can stochastically alpha-test the holes.
                let (tex, cov): ([u8; 4], u32) = match atlas {
                    Some(a) => {
                        let to_px = |u: u32, vv: u32| -> [u8; 4] {
                            let px = ((u as f32 / 65535.0 * a.width() as f32) as u32)
                                .min(a.width().saturating_sub(1));
                            let py = ((vv as f32 / 65535.0 * a.height() as f32) as u32)
                                .min(a.height().saturating_sub(1));
                            a.get_pixel(px, py).0
                        };
                        let cu = (v0.uv[0] as u32 + v1.uv[0] as u32 + v2.uv[0] as u32) / 3;
                        let cv = (v0.uv[1] as u32 + v1.uv[1] as u32 + v2.uv[1] as u32) / 3;
                        let c = to_px(cu, cv);
                        if c[3] >= 128 {
                            (c, 31) // opaque centroid (the common solid-block case)
                        } else {
                            let umin = v0.uv[0].min(v1.uv[0]).min(v2.uv[0]) as u32;
                            let umax = v0.uv[0].max(v1.uv[0]).max(v2.uv[0]) as u32;
                            let vmin = v0.uv[1].min(v1.uv[1]).min(v2.uv[1]) as u32;
                            let vmax = v0.uv[1].max(v1.uv[1]).max(v2.uv[1]) as u32;
                            let mut s = [0u32; 3];
                            let mut wsum = 0u32;
                            for iy in 0..4u32 {
                                for ix in 0..4u32 {
                                    let u = umin + (umax - umin) * (ix * 2 + 1) / 8;
                                    let vv = vmin + (vmax - vmin) * (iy * 2 + 1) / 8;
                                    let p = to_px(u, vv);
                                    let aw = p[3] as u32;
                                    s[0] += p[0] as u32 * aw;
                                    s[1] += p[1] as u32 * aw;
                                    s[2] += p[2] as u32 * aw;
                                    wsum += aw;
                                }
                            }
                            // Average alpha over the 16 samples → coverage 0..31.
                            let cov = (wsum * 31 / (16 * 255)).min(31);
                            if wsum > 0 {
                                ([(s[0] / wsum) as u8, (s[1] / wsum) as u8, (s[2] / wsum) as u8, 255], cov)
                            } else {
                                ([255, 255, 255, 255], 0)
                            }
                        }
                    }
                    None => ([255, 255, 255, 255], 31),
                };
                // Glass / water are non-opaque surfaces; give them full coverage (handled
                // by their material flag, not the alpha test).
                let cov = if material != 0 { 31 } else { cov };
                let r = tex[0] as u32 * tint[0] as u32 / 255;
                let g = tex[1] as u32 * tint[1] as u32 / 255;
                let b = tex[2] as u32 * tint[2] as u32 / 255;
                let emissive = ((v0.pos_light[3] >> 16) & 1) as u32;
                (cov << 27) | mat_bits | (emissive << 24) | (r << 16) | (g << 8) | b
            })
            .collect();
        // Per-triangle packed normal (offset-binary 8:8:8), same index as tri_colors.
        // Computed from the triangle GEOMETRY (edge cross product), not the vertex normal
        // attribute — greedy-meshed cubes repurpose the normal slot for the atlas tile
        // origin, so reading it gives garbage for stone/dirt/etc. The geometric normal is
        // correct for both greedy and regular geometry.
        let tri_normals: Vec<u32> = (0..tri_count_usize)
            .map(|t| {
                let p0 = positions[indices[t * 3] as usize];
                let p1 = positions[indices[t * 3 + 1] as usize];
                let p2 = positions[indices[t * 3 + 2] as usize];
                let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
                let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
                let n = [
                    e1[1] * e2[2] - e1[2] * e2[1],
                    e1[2] * e2[0] - e1[0] * e2[2],
                    e1[0] * e2[1] - e1[1] * e2[0],
                ];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
                let nb = |x: f32| ((x / len * 127.0).round() as i32 + 128).clamp(0, 255) as u32;
                (nb(n[0]) << 16) | (nb(n[1]) << 8) | nb(n[2])
            })
            .collect();
        // Per-triangle atlas UVs: the 3 vertices' Unorm16 UVs, packed 2-per-u32, 3 u32 per
        // triangle (interleaved) so the path tracer can barycentric-interpolate the hit UV.
        let tri_uvs: Vec<u32> = (0..tri_count_usize)
            .flat_map(|t| {
                let pack = |uv: [u16; 2]| (uv[0] as u32) | ((uv[1] as u32) << 16);
                [
                    pack(verts[indices[t * 3] as usize].uv),
                    pack(verts[indices[t * 3 + 1] as usize].uv),
                    pack(verts[indices[t * 3 + 2] as usize].uv),
                ]
            })
            .collect();
        // Per-triangle vertex positions (section-local), 9 f32 per triangle, so the path
        // tracer computes barycentrics from the hit point itself.
        let tri_positions: Vec<f32> = (0..tri_count_usize)
            .flat_map(|t| {
                let p0 = positions[indices[t * 3] as usize];
                let p1 = positions[indices[t * 3 + 1] as usize];
                let p2 = positions[indices[t * 3 + 2] as usize];
                [p0[0], p0[1], p0[2], p1[0], p1[1], p1[2], p2[0], p2[1], p2[2]]
            })
            .collect();

        let vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (positions.len() * std::mem::size_of::<[f32; 3]>()) as u64,
            usage: wgpu::BufferUsages::BLAS_INPUT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vertex_buf, 0, bytemuck::cast_slice(&positions));
        let index_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (indices.len() * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::BLAS_INPUT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&index_buf, 0, bytemuck::cast_slice(&indices));

        let size = wgpu::BlasTriangleGeometrySizeDescriptor {
            vertex_format: wgpu::VertexFormat::Float32x3,
            vertex_count: positions.len() as u32,
            index_format: Some(wgpu::IndexFormat::Uint32),
            index_count: Some(indices.len() as u32),
            // OPAQUE is required — naga only commits opaque hits, so a non-opaque
            // BLAS would never register an intersection in WGSL.
            flags: wgpu::AccelerationStructureGeometryFlags::OPAQUE,
        };
        let blas = device.create_blas(
            &wgpu::CreateBlasDescriptor {
                label: Some(label),
                flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                update_mode: wgpu::AccelerationStructureUpdateMode::Build,
            },
            wgpu::BlasGeometrySizeDescriptors::Triangles {
                descriptors: vec![size.clone()],
            },
        );
        Some(BlasGeom {
            blas,
            vertex_buf,
            index_buf,
            size,
            tri_colors,
            tri_normals,
            tri_uvs,
            tri_positions,
        })
    }

    pub fn remove_section(&mut self, pos: SectionPos) {
        self.sections.remove(&pos);
        self.dirty.retain(|&p| p != pos);
    }

    /// Record this frame's acceleration-structure build into `encoder`: refresh the
    /// TLAS instances (camera-relative transforms for in-range sections) and build
    /// any sections whose BLAS is still pending. Must run before the world pass that
    /// reads the TLAS. `camera_origin` is the render origin in blocks (matches the
    /// chunk shader's `camera.origin`).
    pub fn build(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        camera_origin: IVec3,
    ) {
        let range2 = RT_RANGE_BLOCKS * RT_RANGE_BLOCKS;
        // 0. Gather the nearest in-range emissive lights into the point-light buffer
        // (camera-relative positions, matching the chunk shader's world_pos). The chunk
        // RT shader loops over these for ray-traced point lighting.
        let mut lights: Vec<(f32, GpuLight)> = Vec::new();
        let cam_f = camera_origin.as_vec3();
        for sec in self.sections.values() {
            let so = sec.origin.as_vec3() - cam_f;
            for l in &sec.lights {
                let p = [
                    so.x + l.local_pos[0],
                    so.y + l.local_pos[1],
                    so.z + l.local_pos[2],
                ];
                let d2 = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
                if d2 > range2 {
                    continue;
                }
                lights.push((
                    d2,
                    GpuLight {
                        pos_radius: [p[0], p[1], p[2], l.radius],
                        // Intensity scales with the light level (radius); tunable base.
                        color: [l.color[0], l.color[1], l.color[2], 4.0 * (l.radius / 15.0)],
                    },
                ));
            }
        }
        // Keep the nearest MAX_LIGHTS; zero-fill the rest (intensity 0 = empty slot).
        lights.sort_by(|a, b| a.0.total_cmp(&b.0));
        lights.truncate(MAX_LIGHTS);
        let mut gpu = [GpuLight::zeroed(); MAX_LIGHTS];
        for (i, (_, l)) in lights.iter().enumerate() {
            gpu[i] = *l;
        }
        queue.write_buffer(&self.light_buffer, 0, bytemuck::cast_slice(&gpu));

        // 1. Place in-range sections into the TLAS as translated instances.
        let mut count = 0u32;
        for sec in self.sections.values() {
            if count >= self.max_instances {
                break;
            }
            // 3D distance from the camera to the section centre (blocks).
            let centre = sec.origin + IVec3::splat(8);
            let d = (centre - camera_origin).as_vec3();
            if d.length_squared() > range2 {
                continue;
            }
            let t = sec.origin - camera_origin;
            let (tx, ty, tz) = (t.x as f32, t.y as f32, t.z as f32);
            // 3x4 row-major affine: identity rotation + translation into camera space.
            let transform = [
                1.0, 0.0, 0.0, tx, //
                0.0, 1.0, 0.0, ty, //
                0.0, 0.0, 1.0, tz, //
            ];
            if let Some(s) = &sec.solid {
                self.tlas[count as usize] =
                    Some(wgpu::TlasInstance::new(&s.blas, transform, s.tri_base, 0x01));
                count += 1;
            }
            // Stained-glass instance (ray-masked 0x02): point-light shadow rays sample its
            // colour to tint light; the solid rays (mask 0x01) don't see it.
            if let Some(g) = &sec.glass {
                if count < self.max_instances {
                    self.tlas[count as usize] =
                        Some(wgpu::TlasInstance::new(&g.blas, transform, g.tri_base, 0x02));
                    count += 1;
                }
            }
            // Water instance (ray-masked 0x04): only the path tracer traces it.
            if let Some(w) = &sec.water {
                if count < self.max_instances {
                    self.tlas[count as usize] =
                        Some(wgpu::TlasInstance::new(&w.blas, transform, w.tri_base, 0x04));
                    count += 1;
                }
            }
        }
        // Clear instance slots used last frame but not this one.
        for i in count..self.prev_instance_count {
            self.tlas[i as usize] = None;
        }
        self.prev_instance_count = count;

        // 2. Build pending BLAS (each once) + the TLAS. A BLAS may be built and used
        // by the TLAS in the same call; already-built BLAS need not be rebuilt.
        let geom_entry = |blas, size, vbuf, ibuf| wgpu::BlasBuildEntry {
            blas,
            geometry: wgpu::BlasGeometries::TriangleGeometries(vec![
                wgpu::BlasTriangleGeometry {
                    size,
                    vertex_buffer: vbuf,
                    first_vertex: 0,
                    vertex_stride: std::mem::size_of::<[f32; 3]>() as u64,
                    index_buffer: Some(ibuf),
                    first_index: Some(0),
                    transform_buffer: None,
                    transform_buffer_offset: None,
                },
            ]),
        };
        let entries: Vec<wgpu::BlasBuildEntry> = self
            .dirty
            .iter()
            .filter_map(|pos| self.sections.get(pos))
            .flat_map(|sec| {
                let mut v = Vec::new();
                if let Some(s) = &sec.solid {
                    v.push(geom_entry(&s.blas, &s.size, &s.vertex_buf, &s.index_buf));
                }
                if let Some(g) = &sec.glass {
                    v.push(geom_entry(&g.blas, &g.size, &g.vertex_buf, &g.index_buf));
                }
                if let Some(w) = &sec.water {
                    v.push(geom_entry(&w.blas, &w.size, &w.vertex_buf, &w.index_buf));
                }
                v
            })
            .collect();
        encoder.build_acceleration_structures(entries.iter(), std::iter::once(&self.tlas));
        drop(entries);
        self.dirty.clear();
    }
}
