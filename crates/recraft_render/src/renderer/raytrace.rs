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

use crate::{ChunkMeshBuffers, ChunkVertex};

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

/// One section's ray-tracing geometry + its BLAS. `size` is kept so the per-frame
/// build can describe the geometry without re-deriving it.
struct RtSection {
    blas: wgpu::Blas,
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    size: wgpu::BlasTriangleGeometrySizeDescriptor,
    /// Section world origin in blocks; the TLAS instance translates by this minus
    /// the camera origin each frame.
    origin: IVec3,
    /// Average vertex colour (RGB), packed 8:8:8 into the 24-bit TLAS instance custom
    /// data so a ray that hits this section can tint its GI bounce by the surface
    /// Per-triangle packed colour (8:8:8 RGB) for this section, in BLAS primitive
    /// order. Kept CPU-side so the shared colour pool can be re-packed when it fills.
    tri_colors: Vec<u32>,
    /// This section's base offset into the shared `tri_color_pool` — written into the
    /// 24-bit TLAS instance custom data so a ray hit can fetch the exact triangle's
    /// colour via `tri_color_pool[custom_data + primitive_index]`.
    tri_base: u32,
}

/// Capacity (in triangles / u32s) of the shared per-triangle colour pool. 16M is the
/// 24-bit instance-custom-data ceiling; well above the in-range triangle count.
const TRI_POOL_CAPACITY: u32 = 1 << 23; // 8M triangles = 32 MiB

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
    tri_top: u32,
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
            tri_top: 0,
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
            sec.tri_base = top;
            queue.write_buffer(
                &self.tri_color_pool,
                top as u64 * 4,
                bytemuck::cast_slice(&sec.tri_colors),
            );
            top += sec.tri_colors.len() as u32;
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
    ) {
        let vertex_count = solid.vertices.len() + cutout.vertices.len();
        let index_count = solid.indices.len() + cutout.indices.len();
        if index_count == 0 {
            // No opaque geometry (e.g. a section of only water/glass) — nothing to
            // trace against; drop any prior BLAS so it stops appearing in the TLAS.
            self.remove_section(pos);
            return;
        }
        let origin = IVec3::new(pos.x * 16, pos.y * 16, pos.z * 16);
        let (ox, oy, oz) = (origin.x as f32, origin.y as f32, origin.z as f32);
        // Section-local float positions (decode the fixed-point ×64 i32 to blocks,
        // then make it section-relative so the magnitudes stay in ~[0, 16]).
        let to_local = |v: &ChunkVertex| -> [f32; 3] {
            [
                v.pos_light[0] as f32 / 64.0 - ox,
                v.pos_light[1] as f32 / 64.0 - oy,
                v.pos_light[2] as f32 / 64.0 - oz,
            ]
        };
        let mut positions: Vec<[f32; 3]> = Vec::with_capacity(vertex_count);
        positions.extend(solid.vertices.iter().map(&to_local));
        positions.extend(cutout.vertices.iter().map(&to_local));
        // u32 indices (cutout indices shifted past the solid vertices). u32 avoids
        // overflow when solid+cutout exceed 65 536 verts, unlike the u16 render path.
        let mut indices: Vec<u32> = Vec::with_capacity(index_count);
        indices.extend(solid.indices.iter().map(|&i| i as u32));
        let base = solid.vertices.len() as u32;
        indices.extend(cutout.indices.iter().map(|&i| i as u32 + base));

        // Per-triangle surface colour (8:8:8 RGB) in BLAS primitive order — fetched at a
        // ray hit via tri_color_pool[instance_custom_data + primitive_index] for per-block
        // GI colour bleeding. The colour is the atlas texel at the triangle's centroid UV
        // (the real block colour) times the vertex tint (biome colour × face shade); the
        // vertex colour alone is just the tint, white for most blocks.
        let solid_n = solid.vertices.len();
        let vert = |i: u32| -> &ChunkVertex {
            let i = i as usize;
            if i < solid_n {
                &solid.vertices[i]
            } else {
                &cutout.vertices[i - solid_n]
            }
        };
        let tri_count_usize = indices.len() / 3;
        let tri_colors: Vec<u32> = {
            let atlas = self.atlas.as_ref();
            (0..tri_count_usize)
                .map(|t| {
                    let v0 = vert(indices[t * 3]);
                    let tint = v0.color;
                    let v1 = vert(indices[t * 3 + 1]);
                    let v2 = vert(indices[t * 3 + 2]);
                    let tex = match atlas {
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
                                c // opaque centroid (the common solid-block case)
                            } else {
                                // Cutout hole at the centroid (flowers/grass/leaves):
                                // alpha-weighted average over the triangle's UV box so the
                                // colour is the real petal/leaf colour, not a black void.
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
                                if wsum > 0 {
                                    [(s[0] / wsum) as u8, (s[1] / wsum) as u8, (s[2] / wsum) as u8, 255]
                                } else {
                                    [255, 255, 255, 255]
                                }
                            }
                        }
                        None => [255, 255, 255, 255],
                    };
                    let r = tex[0] as u32 * tint[0] as u32 / 255;
                    let g = tex[1] as u32 * tint[1] as u32 / 255;
                    let b = tex[2] as u32 * tint[2] as u32 / 255;
                    (r << 16) | (g << 8) | b
                })
                .collect()
        };
        // Bump-allocate a range in the shared pool, re-packing if it would overflow.
        let tri_count = tri_colors.len() as u32;
        if self.tri_top + tri_count > TRI_POOL_CAPACITY {
            self.repack(queue);
        }
        let tri_base = self.tri_top;
        self.tri_top += tri_count;
        if tri_count > 0 {
            queue.write_buffer(
                &self.tri_color_pool,
                tri_base as u64 * 4,
                bytemuck::cast_slice(&tri_colors),
            );
        }

        let vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt-section-verts"),
            size: (positions.len() * std::mem::size_of::<[f32; 3]>()) as u64,
            usage: wgpu::BufferUsages::BLAS_INPUT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vertex_buf, 0, bytemuck::cast_slice(&positions));
        let index_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt-section-indices"),
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
                label: Some("rt-section-blas"),
                flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                update_mode: wgpu::AccelerationStructureUpdateMode::Build,
            },
            wgpu::BlasGeometrySizeDescriptors::Triangles {
                descriptors: vec![size.clone()],
            },
        );

        self.sections.insert(
            pos,
            RtSection {
                blas,
                vertex_buf,
                index_buf,
                size,
                origin,
                tri_colors,
                tri_base,
            },
        );
        self.dirty.push(pos);
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
    pub fn build(&mut self, encoder: &mut wgpu::CommandEncoder, camera_origin: IVec3) {
        let range2 = RT_RANGE_BLOCKS * RT_RANGE_BLOCKS;
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
            self.tlas[count as usize] =
                Some(wgpu::TlasInstance::new(&sec.blas, transform, sec.tri_base, 0xFF));
            count += 1;
        }
        // Clear instance slots used last frame but not this one.
        for i in count..self.prev_instance_count {
            self.tlas[i as usize] = None;
        }
        self.prev_instance_count = count;

        // 2. Build pending BLAS (each once) + the TLAS. A BLAS may be built and used
        // by the TLAS in the same call; already-built BLAS need not be rebuilt.
        let entries: Vec<wgpu::BlasBuildEntry> = self
            .dirty
            .iter()
            .filter_map(|pos| {
                let sec = self.sections.get(pos)?;
                Some(wgpu::BlasBuildEntry {
                    blas: &sec.blas,
                    geometry: wgpu::BlasGeometries::TriangleGeometries(vec![
                        wgpu::BlasTriangleGeometry {
                            size: &sec.size,
                            vertex_buffer: &sec.vertex_buf,
                            first_vertex: 0,
                            vertex_stride: std::mem::size_of::<[f32; 3]>() as u64,
                            index_buffer: Some(&sec.index_buf),
                            first_index: Some(0),
                            transform_buffer: None,
                            transform_buffer_offset: None,
                        },
                    ]),
                })
            })
            .collect();
        encoder.build_acceleration_structures(entries.iter(), std::iter::once(&self.tlas));
        drop(entries);
        self.dirty.clear();
    }
}
