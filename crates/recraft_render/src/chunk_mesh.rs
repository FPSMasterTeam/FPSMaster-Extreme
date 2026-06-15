use bytemuck::{Pod, Zeroable};
use recraft_core::{
    collision, door_box, BlockFace, BlockState, Chunk, ChunkPos, RenderLayer, RenderShape,
    SectionPos, Tint, World,
};

use crate::texture::STAINED_COLORS;
use crate::AtlasUv;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    /// Material tint × directional face shade × ambient occlusion, multiplied
    /// with the sampled texel; alpha lets leaves/glass blend. The day/night
    /// light level is applied separately in the shader via `light`.
    pub color: [f32; 4],
    pub uv: [f32; 2],
    /// Per-vertex `(sky_light, block_light)` brightness curves (each 0..1). The
    /// shader combines them with the time-of-day sky factor —
    /// `max(sky * sky_brightness, block)` — so sky-lit surfaces darken at night
    /// while torch/lava-lit ones stay lit. Non-world geometry (items, GUI cubes,
    /// the break overlay) uses `(0, 1)` to render full-bright.
    pub light: [f32; 2],
}

/// `light` value for geometry that should ignore the day/night lightmap and
/// always render at full brightness.
pub const FULLBRIGHT: [f32; 2] = [0.0, 1.0];

impl Vertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 4] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x2, 3 => Float32x2];

    pub fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// Chunk vertex. World-space position is stored as fixed-point i32 at 1/64
/// block resolution. Light curves are packed into the 4th position component.
/// Color is RGBA8 unorm and atlas UVs are 16-bit unorm.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct ChunkVertex {
    /// xyz: world position × 64, as fixed-point i32 (1/64 block precision).
    /// w: packed `(sky_u8 << 8) | block_u8`.
    pub pos_light: [i32; 4],
    /// Material tint × directional face shade × AO + alpha, as RGBA8 unorm.
    pub color: [u8; 4],
    /// Normalized atlas UV as Unorm16×2.
    pub uv: [u16; 2],
    /// Geometric face normal as Snorm8×4 (xyz in −1..1, w unused), derived from
    /// the quad winding — used by the shader lighting pass.
    pub normal: [i8; 4],
}

impl ChunkVertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 4] =
        wgpu::vertex_attr_array![0 => Sint32x4, 1 => Unorm8x4, 2 => Unorm16x2, 3 => Snorm8x4];

    pub fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ChunkMeshBuffers {
    pub vertices: Vec<ChunkVertex>,
    pub indices: Vec<u16>,
}

impl ChunkMeshBuffers {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn push_quad(
        &mut self,
        corners: [[f32; 3]; 4],
        uvs: [[f32; 2]; 4],
        color: [f32; 4],
        light: [f32; 2],
    ) {
        self.push_quad_smooth(corners, uvs, [color; 4], [light; 4]);
    }

    fn push_quad_double_sided(
        &mut self,
        corners: [[f32; 3]; 4],
        uvs: [[f32; 2]; 4],
        color: [f32; 4],
        light: [f32; 2],
    ) {
        self.push_quad(corners, uvs, color, light);
        let back = [corners[3], corners[2], corners[1], corners[0]];
        let back_uvs = [uvs[3], uvs[2], uvs[1], uvs[0]];
        self.push_quad(back, back_uvs, color, light);
    }

    fn push_quad_smooth(
        &mut self,
        corners: [[f32; 3]; 4],
        uvs: [[f32; 2]; 4],
        colors: [[f32; 4]; 4],
        lights: [[f32; 2]; 4],
    ) {
        // Derive the face normal from the winding (CCW front face). All meshed
        // quads are planar, so the cross product of two edges is the geometric
        // normal for cubes, slabs, stairs and (per side) cross-plants.
        let normal = quad_normal(corners);
        let start = self.vertices.len() as u16;
        for (((position, uv), color), light) in
            corners.into_iter().zip(uvs).zip(colors).zip(lights)
        {
            self.vertices
                .push(encode_chunk_vertex(position, color, uv, light, normal));
        }
        self.indices
            .extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
    }
}

/// Unit normal of a planar quad from its first three corners (CCW = front).
fn quad_normal(corners: [[f32; 3]; 4]) -> [f32; 3] {
    let a = corners[0];
    let b = corners[1];
    let c = corners[2];
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 1e-6 {
        [n[0] / len, n[1] / len, n[2] / len]
    } else {
        [0.0, 1.0, 0.0]
    }
}

fn encode_chunk_vertex(
    position: [f32; 3],
    color: [f32; 4],
    uv: [f32; 2],
    light: [f32; 2],
    normal: [f32; 3],
) -> ChunkVertex {
    let snorm = |x: f32| (x.clamp(-1.0, 1.0) * 127.0).round() as i8;
    let px = (position[0] * 64.0).round() as i32;
    let py = (position[1] * 64.0).round() as i32;
    let pz = (position[2] * 64.0).round() as i32;
    let sky_u8 = (light[0] * 255.0 + 0.5) as u8;
    let block_u8 = (light[1] * 255.0 + 0.5) as u8;
    let w = ((sky_u8 as i32) << 8) | (block_u8 as i32);
    ChunkVertex {
        pos_light: [px, py, pz, w],
        color: [
            (color[0] * 255.0 + 0.5) as u8,
            (color[1] * 255.0 + 0.5) as u8,
            (color[2] * 255.0 + 0.5) as u8,
            (color[3] * 255.0 + 0.5) as u8,
        ],
        uv: [
            (uv[0] * 65535.0 + 0.5) as u16,
            (uv[1] * 65535.0 + 0.5) as u16,
        ],
        normal: [snorm(normal[0]), snorm(normal[1]), snorm(normal[2]), 0],
    }
}

/// Per-biome tint colors (0..1) applied to grass and foliage, which ship as
/// grayscale textures in vanilla and must be colored at runtime.
#[derive(Debug, Clone, Copy)]
pub struct BiomeColors {
    pub grass: [f32; 3],
    pub foliage: [f32; 3],
}

impl Default for BiomeColors {
    fn default() -> Self {
        Self {
            grass: [0.569, 0.741, 0.349],
            foliage: [0.467, 0.671, 0.184],
        }
    }
}

/// Plains water tint applied to the grayscale water texture.
const WATER_COLOR: [f32; 3] = [0.247, 0.463, 0.894];

/// Read-only block/light source the mesher walks. Implemented by the live
/// `World` (synchronous path) and by a self-contained `ChunkNeighborhood`
/// snapshot (off-thread worker path).
pub trait BlockSource {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockState;
    fn light_at(&self, x: i32, y: i32, z: i32) -> (u8, u8);
}

impl BlockSource for World {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockState {
        World::block_at(self, x, y, z)
    }
    fn light_at(&self, x: i32, y: i32, z: i32) -> (u8, u8) {
        World::light_at(self, x, y, z)
    }
}

/// Chunk-grid offsets of the 8 chunks surrounding the centre, in storage order.
/// Includes diagonals so the smooth-lighting mesher can sample corner-adjacent
/// blocks across chunk boundaries without seams.
const NEIGHBOR_OFFSETS: [(i32, i32); 8] = [
    (-1, 0),
    (1, 0),
    (0, -1),
    (0, 1),
    (-1, -1),
    (1, -1),
    (-1, 1),
    (1, 1),
];

/// A clone of one chunk plus its eight surrounding neighbours (axis + diagonal)
/// — everything the mesher needs (border-face culling and smooth-lighting AO)
/// to build a chunk mesh on a worker thread without touching the live `World`.
pub struct ChunkNeighborhood {
    pos: ChunkPos,
    center: Chunk,
    /// Surrounding chunks in `NEIGHBOR_OFFSETS` order (None when not loaded).
    neighbors: [Option<Chunk>; 8],
}

impl ChunkNeighborhood {
    /// Snapshot `pos` and its present neighbours out of `world`. Returns None if
    /// the centre chunk isn't loaded. The clones are cheap relative to meshing
    /// and let the actual mesh build happen off the render thread.
    pub fn snapshot(world: &World, pos: ChunkPos) -> Option<Self> {
        let center = world.chunk(pos)?.clone();
        let neighbors = std::array::from_fn(|i| {
            let (dx, dz) = NEIGHBOR_OFFSETS[i];
            world.chunk(ChunkPos::new(pos.x + dx, pos.z + dz)).cloned()
        });
        Some(Self {
            pos,
            center,
            neighbors,
        })
    }

    pub fn position(&self) -> ChunkPos {
        self.pos
    }

    fn chunk_for(&self, cx: i32, cz: i32) -> Option<&Chunk> {
        let offset = (cx - self.pos.x, cz - self.pos.z);
        if offset == (0, 0) {
            return Some(&self.center);
        }
        if let Some(i) = NEIGHBOR_OFFSETS.iter().position(|&o| o == offset) {
            return self.neighbors[i].as_ref();
        }
        None
    }
}

impl BlockSource for ChunkNeighborhood {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockState {
        match self.chunk_for(x.div_euclid(16), z.div_euclid(16)) {
            Some(chunk) => chunk.get_block(x.rem_euclid(16) as u8, y, z.rem_euclid(16) as u8),
            None => BlockState::AIR,
        }
    }
    fn light_at(&self, x: i32, y: i32, z: i32) -> (u8, u8) {
        match self.chunk_for(x.div_euclid(16), z.div_euclid(16)) {
            Some(chunk) => chunk.light_at(x.rem_euclid(16) as u8, y, z.rem_euclid(16) as u8),
            None => (0, 15),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct MeshBuffers {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

/// A chunk's geometry split by render pass: opaque, alpha-tested cutout
/// (leaves/plants/glass — keeps the texture's transparent gaps), and
/// alpha-blended translucent (water/ice/stained glass).
#[derive(Debug, Default, Clone)]
pub struct ChunkMesh {
    pub solid: ChunkMeshBuffers,
    pub cutout: ChunkMeshBuffers,
    pub transparent: ChunkMeshBuffers,
    /// Water surfaces, split out from `transparent` so they can be drawn with a
    /// dedicated water shader (waves + reflection).
    pub water: ChunkMeshBuffers,
}

impl ChunkMesh {
    pub fn is_empty(&self) -> bool {
        self.solid.is_empty()
            && self.cutout.is_empty()
            && self.transparent.is_empty()
            && self.water.is_empty()
    }
}

/// One axis-aligned cube face: outward normal, the four corners in unit cube
/// space (each coordinate 0 or 1), a baked directional shade, and which texture
/// (top/bottom/side) the block uses for it.
struct Face {
    normal: [i32; 3],
    corners: [[f32; 3]; 4],
    light: f32,
    face: BlockFace,
}

const FACES: [Face; 6] = [
    Face {
        normal: [1, 0, 0],
        corners: [
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 1.0, 1.0],
            [1.0, 0.0, 1.0],
        ],
        // Vanilla east/west (±X) face shade.
        light: 0.6,
        face: BlockFace::Side,
    },
    Face {
        normal: [-1, 0, 0],
        corners: [
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 1.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0],
        ],
        // Vanilla east/west (±X) face shade.
        light: 0.6,
        face: BlockFace::Side,
    },
    Face {
        normal: [0, 1, 0],
        corners: [
            [0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        light: 1.00,
        face: BlockFace::Top,
    },
    Face {
        normal: [0, -1, 0],
        corners: [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        ],
        // Vanilla down (−Y) face shade.
        light: 0.5,
        face: BlockFace::Bottom,
    },
    Face {
        normal: [0, 0, 1],
        corners: [
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
            [0.0, 0.0, 1.0],
        ],
        // Vanilla north/south (±Z) face shade.
        light: 0.8,
        face: BlockFace::Side,
    },
    Face {
        normal: [0, 0, -1],
        corners: [
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
        ],
        // Vanilla north/south (±Z) face shade.
        light: 0.8,
        face: BlockFace::Side,
    },
];

pub fn build_world_mesh(
    world: &World,
    atlas: &AtlasUv,
    biome: BiomeColors,
    fast_leaves: bool,
) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    for chunk in world.chunks() {
        append_chunk_mesh(world, chunk, &mut mesh, atlas, biome, fast_leaves);
    }
    mesh
}

/// Build the mesh of a single 16³ section against the live `World` (synchronous
/// path, used by the full-world upload). Neighbour lookups for face culling and
/// smooth lighting read the world directly, so cross-section/cross-chunk borders
/// resolve correctly.
pub fn build_section_mesh(
    world: &World,
    pos: SectionPos,
    atlas: &AtlasUv,
    biome: BiomeColors,
    fast_leaves: bool,
) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    if let Some(chunk) = world.chunk(pos.chunk()) {
        append_section_mesh(world, chunk, pos.y, &mut mesh, atlas, biome, fast_leaves);
    }
    mesh
}

/// Build a single section's mesh from a self-contained neighbourhood snapshot.
/// This is the off-main-thread path: no live `World` is referenced, so it runs
/// on a worker thread. The snapshot is the whole column plus its eight
/// horizontal neighbours, so the up/down sections needed for vertical face
/// culling and smooth lighting are present in `center`.
pub fn build_section_mesh_neighborhood(
    neighborhood: &ChunkNeighborhood,
    section_y: i32,
    atlas: &AtlasUv,
    biome: BiomeColors,
    fast_leaves: bool,
) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    append_section_mesh(
        neighborhood,
        &neighborhood.center,
        section_y,
        &mut mesh,
        atlas,
        biome,
        fast_leaves,
    );
    mesh
}

fn append_chunk_mesh<S: BlockSource>(
    source: &S,
    chunk: &Chunk,
    mesh: &mut ChunkMesh,
    atlas: &AtlasUv,
    biome: BiomeColors,
    fast_leaves: bool,
) {
    for section in chunk.sections() {
        append_section_mesh(source, chunk, section.y(), mesh, atlas, biome, fast_leaves);
    }
}

/// Emit the geometry of section `section_y` (0..16) of `chunk` into `mesh`.
fn append_section_mesh<S: BlockSource>(
    source: &S,
    chunk: &Chunk,
    section_y: i32,
    mesh: &mut ChunkMesh,
    atlas: &AtlasUv,
    biome: BiomeColors,
    fast_leaves: bool,
) {
    let Some(section) = chunk.section(section_y) else {
        return;
    };
    let base_x = chunk.position.x * 16;
    let base_z = chunk.position.z * 16;
    let base_y = section_y * 16;
    for y in 0..16i32 {
        for z in 0..16i32 {
            for x in 0..16i32 {
                let block = section.get(x as u8, y as u8, z as u8);
                if block.is_air() {
                    continue;
                }
                let ctx = BlockCtx {
                    source,
                    chunk,
                    base_x,
                    base_z,
                    atlas,
                    biome,
                    fast_leaves,
                };
                append_block(mesh, &ctx, base_x + x, base_y + y, base_z + z, block);
            }
        }
    }
}

struct BlockCtx<'a, S: BlockSource> {
    source: &'a S,
    chunk: &'a Chunk,
    base_x: i32,
    base_z: i32,
    atlas: &'a AtlasUv,
    biome: BiomeColors,
    /// Fast graphics: merge adjacent same-id leaf faces (canopy shell only).
    fast_leaves: bool,
}

fn append_block<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    match block.render_shape() {
        RenderShape::None => {}
        RenderShape::Cube => append_cube(mesh, ctx, x, y, z, block),
        RenderShape::Cross => append_cross(mesh, ctx, x, y, z, block),
        RenderShape::Rail => append_rail(mesh, ctx, x, y, z, block),
        RenderShape::Ladder => append_ladder(mesh, ctx, x, y, z, block),
        RenderShape::Boxes => append_boxes(mesh, ctx, x, y, z, block),
        RenderShape::Door => append_door(mesh, ctx, x, y, z, block),
        RenderShape::Piston => append_piston(mesh, ctx, x, y, z, block),
        RenderShape::PistonHead => append_piston_head(mesh, ctx, x, y, z, block),
    }
}

/// Outward normal of each piston/door facing index (vanilla `EnumFacing`:
/// 0 down, 1 up, 2 north, 3 south, 4 west, 5 east).
const FACING_NORMAL: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];

fn buffer_for(mesh: &mut ChunkMesh, block: BlockState) -> &mut ChunkMeshBuffers {
    if block.is_water() {
        return &mut mesh.water;
    }
    match block.render_layer() {
        RenderLayer::Solid => &mut mesh.solid,
        RenderLayer::Cutout => &mut mesh.cutout,
        RenderLayer::Translucent => &mut mesh.transparent,
    }
}

/// Full-cube block: emit each face that isn't hidden by an opaque neighbour or
/// merged with an identical neighbour (so e.g. water surfaces don't z-fight).
fn append_cube<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let alpha = block.render_alpha();
    let buffer = buffer_for(mesh, block);
    for face in &FACES {
        let (nx, ny, nz) = (x + face.normal[0], y + face.normal[1], z + face.normal[2]);
        let neighbor = neighbor_block(ctx, nx, ny, nz);
        // Cull faces against an opaque neighbour, and against a same-id neighbour
        // for blocks that merge (glass/ice/water). Leaves are the vanilla Fancy
        // exception: they keep the face between adjacent leaf blocks for the
        // layered, bushy look — but Fast graphics merges them too (just the
        // canopy shell), cutting a lot of forest geometry on weak hardware.
        let merges_same_id = ctx.fast_leaves || !block.is_leaves();
        if neighbor.is_opaque_cube() || (neighbor.id == block.id && merges_same_id) {
            continue;
        }
        emit_face(
            buffer,
            ctx,
            face,
            x,
            y,
            z,
            [0.0; 3],
            [1.0; 3],
            block.texture_name(face.face),
            block,
            alpha,
            nx,
            ny,
            nz,
        );
    }
}

/// Partial-shape block (slab/stairs/snow/fence/pane/…): render each shape
/// box's faces with no neighbour culling. Stairs, fences and panes derive
/// their boxes from the shared vanilla shape logic (neighbour-dependent).
fn append_boxes<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let alpha = block.render_alpha();
    let boxes = shape_boxes(ctx, x, y, z, block);
    let pane_edge = pane_edge_texture(block);
    let buffer = buffer_for(mesh, block);
    for shape_box in &boxes {
        let mn = [
            shape_box[0] as f32,
            shape_box[1] as f32,
            shape_box[2] as f32,
        ];
        let mx = [
            shape_box[3] as f32,
            shape_box[4] as f32,
            shape_box[5] as f32,
        ];
        for face in &FACES {
            let (nx, ny, nz) = (x + face.normal[0], y + face.normal[1], z + face.normal[2]);
            // Pane top/bottom faces use the dedicated edge texture (vanilla
            // glass_pane_top); everything else follows the data file.
            let texture = match (&pane_edge, face.face) {
                (Some(edge), BlockFace::Top | BlockFace::Bottom) => Some(edge.as_str()),
                _ => block.texture_name(face.face),
            };
            emit_face(
                buffer, ctx, face, x, y, z, mn, mx, texture, block, alpha, nx, ny, nz,
            );
        }
    }
}

/// Unit-space `[x0, y0, z0, x1, y1, z1]` boxes for a partial block, using the
/// shared neighbour-aware vanilla shapes for stairs, fences and panes.
fn shape_boxes<S: BlockSource>(
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) -> Vec<[f64; 6]> {
    let id = block.id;
    let lookup = |bx: i32, by: i32, bz: i32| ctx.source.block_at(bx, by, bz);
    if collision::is_stairs(id) {
        return collision::stair_boxes(&lookup, x, y, z);
    }
    if collision::is_fence(id) {
        // Vanilla fence model: 4x16x4 centre post plus two 2px-wide,
        // 3px-tall bars per connected arm (y 12..15 and 6..9).
        let [n, s, w, e] = collision::fence_connections(&lookup, id, x, y, z);
        let mut boxes = vec![[0.375, 0.0, 0.375, 0.625, 1.0, 0.625]];
        let arm = |x0: f64, z0: f64, x1: f64, z1: f64, boxes: &mut Vec<[f64; 6]>| {
            boxes.push([x0, 0.75, z0, x1, 0.9375, z1]);
            boxes.push([x0, 0.375, z0, x1, 0.5625, z1]);
        };
        if n {
            arm(0.4375, 0.0, 0.5625, 0.375, &mut boxes);
        }
        if s {
            arm(0.4375, 0.625, 0.5625, 1.0, &mut boxes);
        }
        if w {
            arm(0.0, 0.4375, 0.375, 0.5625, &mut boxes);
        }
        if e {
            arm(0.625, 0.4375, 1.0, 0.5625, &mut boxes);
        }
        return boxes;
    }
    if collision::is_pane(id) {
        // Vanilla pane: 2px-thick centre post plus a panel per connected arm;
        // an unconnected pane renders the full cross.
        let [n, s, w, e] = collision::pane_connections(&lookup, id, x, y, z);
        let none = !(n || s || w || e);
        let mut boxes = vec![[0.4375, 0.0, 0.4375, 0.5625, 1.0, 0.5625]];
        if n || none {
            boxes.push([0.4375, 0.0, 0.0, 0.5625, 1.0, 0.4375]);
        }
        if s || none {
            boxes.push([0.4375, 0.0, 0.5625, 0.5625, 1.0, 1.0]);
        }
        if w || none {
            boxes.push([0.0, 0.0, 0.4375, 0.4375, 1.0, 0.5625]);
        }
        if e || none {
            boxes.push([0.5625, 0.0, 0.4375, 1.0, 1.0, 0.5625]);
        }
        return boxes;
    }
    block
        .render_boxes()
        .as_slice()
        .iter()
        .map(|b| [b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]])
        .collect()
}

/// The edge texture for pane top/bottom faces (vanilla glass_pane_top); iron
/// bars use their own texture everywhere.
fn pane_edge_texture(block: BlockState) -> Option<String> {
    match block.id {
        102 => Some("glass_pane_top".to_owned()),
        160 => Some(format!(
            "glass_pane_top_{}",
            STAINED_COLORS[(block.meta & 15) as usize]
        )),
        _ => None,
    }
}

/// Emit one quad for `face`, mapping its unit corners into the sub-box
/// [mn,mx] and cropping the texture by the box extents (vanilla element UV
/// semantics, so e.g. a slab side shows the lower half of the texture).
#[allow(clippy::too_many_arguments)]
fn emit_face<S: BlockSource>(
    buffer: &mut ChunkMeshBuffers,
    ctx: &BlockCtx<S>,
    face: &Face,
    x: i32,
    y: i32,
    z: i32,
    mn: [f32; 3],
    mx: [f32; 3],
    texture: Option<&str>,
    block: BlockState,
    alpha: f32,
    nx: i32,
    ny: i32,
    nz: i32,
) {
    let tint = tint_color(block.tint(face.face), ctx.biome);
    warn_if_missing(ctx.atlas, block, face_context(face.face), texture);
    let rect = ctx.atlas.tile_rect(texture);
    let front = [nx, ny, nz];
    let mut corners = [[0.0f32; 3]; 4];
    let mut uvs = [[0.0f32; 2]; 4];
    let mut colors = [[0.0f32; 4]; 4];
    let mut lights = [[0.0f32; 2]; 4];
    for (i, corner) in face.corners.iter().enumerate() {
        let px = lerp_axis(corner[0], mn[0], mx[0]);
        let py = lerp_axis(corner[1], mn[1], mx[1]);
        let pz = lerp_axis(corner[2], mn[2], mx[2]);
        corners[i] = [x as f32 + px, y as f32 + py, z as f32 + pz];
        let (u, v) = face_uv(face.normal, px, py, pz);
        uvs[i] = [rect[0] + u * rect[2], rect[1] + v * rect[3]];
        // Smooth (per-vertex) light: the material color carries the directional
        // face shade and ambient occlusion, while the sky/block light curves go
        // to the `light` attribute so the shader applies the day/night factor.
        let (sky, block, ao) = vertex_light(ctx, face.normal, front, *corner);
        colors[i] = [
            tint[0] * face.light * ao,
            tint[1] * face.light * ao,
            tint[2] * face.light * ao,
            alpha,
        ];
        lights[i] = [sky, block];
    }
    buffer.push_quad_smooth(corners, uvs, colors, lights);
}

/// Surface a magenta-fallback texture resolution in the log (deduplicated per
/// block/meta/face), so broken texture wiring is easy to find.
fn warn_if_missing(atlas: &AtlasUv, block: BlockState, context: &str, name: Option<&str>) {
    if atlas.is_missing_tile(name) {
        crate::texture::warn_missing_tile(block.id, block.meta, context, name);
    }
}

fn face_context(face: BlockFace) -> &'static str {
    match face {
        BlockFace::Top => "the top face",
        BlockFace::Bottom => "the bottom face",
        BlockFace::Side => "a side face",
    }
}

/// Vanilla element UV mapping: u/v fractions within the tile for a point on a
/// face (v grows downward; the north and east faces mirror u so horizontal
/// textures read correctly from outside).
fn face_uv(normal: [i32; 3], px: f32, py: f32, pz: f32) -> (f32, f32) {
    match normal {
        [0, 1, 0] => (px, pz),
        [0, -1, 0] => (px, 1.0 - pz),
        [0, 0, 1] => (px, 1.0 - py),
        [0, 0, -1] => (1.0 - px, 1.0 - py),
        [1, 0, 0] => (1.0 - pz, 1.0 - py),
        _ => (pz, 1.0 - py),
    }
}

/// A unit-cube corner coordinate (0 or 1) mapped into the box extent.
fn lerp_axis(corner: f32, lo: f32, hi: f32) -> f32 {
    if corner < 0.5 {
        lo
    } else {
        hi
    }
}

/// Cross-shaped plant: two diagonal planes, each emitted double-sided so the
/// plant is visible from every direction under back-face culling (the vanilla
/// `cross` model likewise carries a face for each side of each plane).
fn append_cross<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let light = face_light(ctx, x, y, z);
    let tint = tint_color(block.tint(BlockFace::Side), ctx.biome);
    let color = [tint[0], tint[1], tint[2], block.render_alpha()];
    let texture = block.texture_name(BlockFace::Side);
    warn_if_missing(ctx.atlas, block, "its cross texture", texture);
    let uvs = ctx.atlas.uv(texture);
    let (fx, fy, fz) = (x as f32, y as f32, z as f32);
    // Vanilla cross model: the diagonals are inset 0.8/16 from the corners
    // (rotated 45° with rescale), not stretched corner-to-corner.
    let lo = 0.05;
    let hi = 0.95;
    // Corner order matches the UV table convention (bottom, top, top, bottom)
    // so the texture stands upright instead of twisting across the quad.
    let buffer = buffer_for(mesh, block);
    buffer.push_quad_double_sided(
        [
            [fx + lo, fy, fz + lo],
            [fx + lo, fy + 1.0, fz + lo],
            [fx + hi, fy + 1.0, fz + hi],
            [fx + hi, fy, fz + hi],
        ],
        uvs,
        color,
        light,
    );
    buffer.push_quad_double_sided(
        [
            [fx + hi, fy, fz + lo],
            [fx + hi, fy + 1.0, fz + lo],
            [fx + lo, fy + 1.0, fz + hi],
            [fx + lo, fy, fz + hi],
        ],
        uvs,
        color,
        light,
    );
}

/// Rail: a flat (or sloped) quad 1/16 above the floor. The texture's V axis
/// runs along the track, so east-west pieces rotate the UVs a quarter turn;
/// curves use the turned texture with the vanilla blockstate rotations
/// (south-east = 0°, then clockwise per quarter).
fn append_rail<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    // Powered/detector/activator rails keep the powered bit out of the shape.
    let shape = if block.id == 66 {
        block.meta & 15
    } else {
        block.meta & 0x7
    };
    let light = face_light(ctx, x, y + 1, z);
    let tint = tint_color(block.tint(BlockFace::Top), ctx.biome);
    let color = [tint[0], tint[1], tint[2], block.render_alpha()];

    let (fx, fy, fz) = (x as f32, y as f32, z as f32);
    let low = fy + 0.0625;
    // Ascending rails rise a full block so they meet the rail above (vanilla
    // raised-rail model top edge at 17/16).
    let high = fy + 1.0625;
    // Corner heights in NW, NE, SE, SW order.
    let mut heights = [low; 4];
    match shape {
        2 => {
            heights[1] = high; // ascending east: +x edge raised
            heights[2] = high;
        }
        3 => {
            heights[0] = high; // ascending west
            heights[3] = high;
        }
        4 => {
            heights[0] = high; // ascending north (-z)
            heights[1] = high;
        }
        5 => {
            heights[2] = high; // ascending south
            heights[3] = high;
        }
        _ => {}
    }
    let corners = [
        [fx, heights[0], fz],
        [fx + 1.0, heights[1], fz],
        [fx + 1.0, heights[2], fz + 1.0],
        [fx, heights[3], fz + 1.0],
    ];

    // Clockwise quarter-turns viewed from above.
    let turns = match shape {
        1..=3 => 1, // east-west straight/slopes: track runs along x
        7 => 1,     // south-west curve
        8 => 2,     // north-west curve
        9 => 3,     // north-east curve
        _ => 0,     // north-south pieces and the south-east curve
    };
    let texture = block.texture_name(BlockFace::Top);
    warn_if_missing(ctx.atlas, block, "its rail texture", texture);
    let rect = ctx.atlas.tile_rect(texture);
    let mut uvs = [
        [rect[0], rect[1]],
        [rect[0] + rect[2], rect[1]],
        [rect[0] + rect[2], rect[1] + rect[3]],
        [rect[0], rect[1] + rect[3]],
    ];
    uvs.rotate_right(turns);
    // Double-sided so the track is visible from above and (through a glass
    // floor) below, under back-face culling.
    buffer_for(mesh, block).push_quad_double_sided(corners, uvs, color, light);
}

fn append_ladder<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let (corners, sample) = ladder_quad(x, y, z, block.meta);
    let light = face_light(ctx, x + sample[0], y + sample[1], z + sample[2]);
    let tint = tint_color(block.tint(BlockFace::Side), ctx.biome);
    let color = [tint[0], tint[1], tint[2], block.render_alpha()];
    let texture = block.texture_name(BlockFace::Side);
    warn_if_missing(ctx.atlas, block, "its ladder texture", texture);
    let uvs = ctx.atlas.uv(texture);
    // Double-sided so the rungs show under back-face culling regardless of which
    // way the single quad happens to wind.
    buffer_for(mesh, block).push_quad_double_sided(corners, uvs, color, light);
}

fn ladder_quad(x: i32, y: i32, z: i32, meta: u8) -> ([[f32; 3]; 4], [i32; 3]) {
    let (fx, fy, fz) = (x as f32, y as f32, z as f32);
    let inset = 0.01;
    match meta {
        3 => {
            let p = fz + inset;
            (
                [
                    [fx, fy, p],
                    [fx, fy + 1.0, p],
                    [fx + 1.0, fy + 1.0, p],
                    [fx + 1.0, fy, p],
                ],
                [0, 0, -1],
            )
        }
        4 => {
            let p = fx + 1.0 - inset;
            (
                [
                    [p, fy, fz + 1.0],
                    [p, fy + 1.0, fz + 1.0],
                    [p, fy + 1.0, fz],
                    [p, fy, fz],
                ],
                [1, 0, 0],
            )
        }
        5 => {
            let p = fx + inset;
            (
                [
                    [p, fy, fz],
                    [p, fy + 1.0, fz],
                    [p, fy + 1.0, fz + 1.0],
                    [p, fy, fz + 1.0],
                ],
                [-1, 0, 0],
            )
        }
        _ => {
            let p = fz + 1.0 - inset;
            (
                [
                    [fx + 1.0, fy, p],
                    [fx + 1.0, fy + 1.0, p],
                    [fx, fy + 1.0, p],
                    [fx, fy, p],
                ],
                [0, 0, 1],
            )
        }
    }
}

/// Door (wood/iron): a 3/16 panel on the edge given by the combined two-half
/// state. The lower half carries facing+open and the upper half the hinge, so
/// each half reads its sibling to resolve the full state (vanilla
/// `BlockDoor.getActualState`). The lower half textures from `door_*_lower`,
/// the upper from `door_*_upper`.
fn append_door<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let upper = block.meta & 8 != 0;
    let (facing, open, hinge_right) = if upper {
        // Hinge is local; facing/open come from the lower half below.
        let hinge_right = block.meta & 1 != 0;
        let lower = neighbor_block(ctx, x, y - 1, z);
        if lower.id == block.id {
            (lower.meta & 3, lower.meta & 4 != 0, hinge_right)
        } else {
            (block.meta & 3, false, hinge_right)
        }
    } else {
        // Facing/open are local; the hinge comes from the upper half above.
        let above = neighbor_block(ctx, x, y + 1, z);
        let hinge_right = above.id == block.id && above.meta & 1 != 0;
        (block.meta & 3, block.meta & 4 != 0, hinge_right)
    };

    let bx = door_box(facing, open, hinge_right);
    let mn = [bx.min[0] as f32, bx.min[1] as f32, bx.min[2] as f32];
    let mx = [bx.max[0] as f32, bx.max[1] as f32, bx.max[2] as f32];
    // Upper half samples door_*_upper (the def's `top`), lower the `bottom`.
    let texture = block.texture_name(if upper { BlockFace::Top } else { BlockFace::Bottom });
    let alpha = block.render_alpha();
    let buffer = buffer_for(mesh, block);
    for face in &FACES {
        let (nx, ny, nz) = (x + face.normal[0], y + face.normal[1], z + face.normal[2]);
        emit_face(
            buffer, ctx, face, x, y, z, mn, mx, texture, block, alpha, nx, ny, nz,
        );
    }
}

/// Piston body (normal/sticky): a full cube whose front face (the `facing`
/// direction) is the piston top when retracted or the recessed `piston_inner`
/// when extended; the opposite face is `piston_bottom` and the rest
/// `piston_side`. Faces cull against opaque neighbours like any cube.
fn append_piston<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let facing = (block.meta & 7) as usize;
    let extended = block.meta & 8 != 0;
    let front = FACING_NORMAL[facing];
    let back = [-front[0], -front[1], -front[2]];
    let front_tex = if extended {
        Some("piston_inner")
    } else {
        block.texture_name(BlockFace::Top)
    };
    let back_tex = block.texture_name(BlockFace::Bottom);
    let side_tex = block.texture_name(BlockFace::Side);
    let alpha = block.render_alpha();
    let buffer = buffer_for(mesh, block);
    for face in &FACES {
        let (nx, ny, nz) = (x + face.normal[0], y + face.normal[1], z + face.normal[2]);
        if neighbor_block(ctx, nx, ny, nz).is_opaque_cube() {
            continue;
        }
        let texture = if face.normal == front {
            front_tex
        } else if face.normal == back {
            back_tex
        } else {
            side_tex
        };
        emit_face(
            buffer,
            ctx,
            face,
            x,
            y,
            z,
            [0.0; 3],
            [1.0; 3],
            texture,
            block,
            alpha,
            nx,
            ny,
            nz,
        );
    }
}

/// Extended piston head (block 34): the 4/16 head plate at the `facing` end
/// (platform texture on its outer face) plus the 4×4 arm reaching back toward
/// the body. Sticky bit (meta 8) selects the sticky platform texture.
fn append_piston_head<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let facing = (block.meta & 7) as usize;
    let sticky = block.meta & 8 != 0;
    let front = FACING_NORMAL[facing];
    // The def stores piston_top_normal in `top` and piston_top_sticky in
    // `bottom` so both reach the atlas; pick per the sticky bit.
    let head_tex = block.texture_name(if sticky { BlockFace::Bottom } else { BlockFace::Top });
    let side_tex = block.texture_name(BlockFace::Side);

    let axis = match facing {
        0 | 1 => 1, // up/down → y
        2 | 3 => 2, // north/south → z
        _ => 0,     // west/east → x
    };
    let positive = matches!(facing, 1 | 3 | 5);
    // Plate: outer 4/16 on the facing side, full cross-section. Arm: inner 12/16,
    // 4×4 cross-section.
    let (plate_lo, plate_hi, arm_lo, arm_hi) = if positive {
        (0.75, 1.0, 0.0, 0.75)
    } else {
        (0.0, 0.25, 0.25, 1.0)
    };
    let plate = axis_box(axis, plate_lo, plate_hi, 0.0, 1.0);
    let arm = axis_box(axis, arm_lo, arm_hi, 0.375, 0.625);

    let alpha = block.render_alpha();
    let buffer = buffer_for(mesh, block);
    for (b, is_plate) in [(plate, true), (arm, false)] {
        let mn = [b[0] as f32, b[1] as f32, b[2] as f32];
        let mx = [b[3] as f32, b[4] as f32, b[5] as f32];
        for face in &FACES {
            let (nx, ny, nz) = (x + face.normal[0], y + face.normal[1], z + face.normal[2]);
            // The plate's outer face shows the platform; everything else is the
            // piston side.
            let texture = if is_plate && face.normal == front {
                head_tex
            } else {
                side_tex
            };
            emit_face(
                buffer, ctx, face, x, y, z, mn, mx, texture, block, alpha, nx, ny, nz,
            );
        }
    }
}

/// A unit-space box spanning `[lo,hi]` on `axis` and `[cross_lo,cross_hi]` on
/// the other two axes, as `[x0,y0,z0,x1,y1,z1]`.
fn axis_box(axis: usize, lo: f64, hi: f64, cross_lo: f64, cross_hi: f64) -> [f64; 6] {
    let mut mn = [cross_lo; 3];
    let mut mx = [cross_hi; 3];
    mn[axis] = lo;
    mx[axis] = hi;
    [mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]]
}

fn tint_color(tint: Tint, biome: BiomeColors) -> [f32; 3] {
    match tint {
        Tint::None => [1.0, 1.0, 1.0],
        Tint::Grass => biome.grass,
        Tint::Foliage => biome.foliage,
        Tint::Water => WATER_COLOR,
        Tint::Rgb(rgb) => rgb,
    }
}

/// Read a neighbouring block, using the owning chunk directly when inside it
/// (the common case) and only falling back to the world hash-map at borders.
#[inline]
fn neighbor_block<S: BlockSource>(ctx: &BlockCtx<S>, x: i32, y: i32, z: i32) -> BlockState {
    let lx = x - ctx.base_x;
    let lz = z - ctx.base_z;
    if (0..16).contains(&lx) && (0..16).contains(&lz) {
        ctx.chunk.get_block(lx as u8, y, lz as u8)
    } else {
        ctx.source.block_at(x, y, z)
    }
}

#[inline]
fn neighbor_light<S: BlockSource>(ctx: &BlockCtx<S>, x: i32, y: i32, z: i32) -> (u8, u8) {
    let lx = x - ctx.base_x;
    let lz = z - ctx.base_z;
    if (0..16).contains(&lx) && (0..16).contains(&lz) {
        ctx.chunk.light_at(lx as u8, y, lz as u8)
    } else {
        ctx.source.light_at(x, y, z)
    }
}

/// Normalize a 0..15 light level to 0..1 for the shader's brightness curve.
#[inline]
fn light_curve(level: f32) -> f32 {
    level / 15.0
}

/// Flat per-face light: the light of the single block in front of the face,
/// as a `(sky_curve, block_curve)` pair so the shader applies the day/night
/// factor. Used by the non-cube shapes (cross plants, rails, ladders) where
/// vanilla does not apply smooth lighting.
fn face_light<S: BlockSource>(ctx: &BlockCtx<S>, x: i32, y: i32, z: i32) -> [f32; 2] {
    let (block_light, sky_light) = neighbor_light(ctx, x, y, z);
    [light_curve(sky_light as f32), light_curve(block_light as f32)]
}

/// Vanilla-style smooth lighting for one corner of a block face: averages the
/// light of the four cells touching the corner in the face's outer layer and
/// the ambient occlusion from the three corner-adjacent blocks. Returns
/// `(sky_curve, block_curve, ao)`: the sky/block light curves (combined in the
/// shader with the day/night factor) and the occlusion multiplier (baked into
/// the vertex color).
fn vertex_light<S: BlockSource>(
    ctx: &BlockCtx<S>,
    normal: [i32; 3],
    front: [i32; 3],
    corner: [f32; 3],
) -> (f32, f32, f32) {
    // The two in-plane (tangent) axes — those the face normal doesn't run along.
    let (a, b) = if normal[0] != 0 {
        (1, 2)
    } else if normal[1] != 0 {
        (0, 2)
    } else {
        (0, 1)
    };
    let sa = if corner[a] >= 0.5 { 1 } else { -1 };
    let sb = if corner[b] >= 0.5 { 1 } else { -1 };

    let mut side1 = front;
    side1[a] += sa;
    let mut side2 = front;
    side2[b] += sb;
    let mut diag = front;
    diag[a] += sa;
    diag[b] += sb;

    let opaque = |p: [i32; 3]| neighbor_block(ctx, p[0], p[1], p[2]).is_opaque_cube();
    let o1 = opaque(side1);
    let o2 = opaque(side2);
    // When both sides are opaque the diagonal cell is hidden behind them.
    let oc = (o1 && o2) || opaque(diag);

    // Light average, matching vanilla `getAoBrightness`, but keeping sky and
    // block light separate so the shader can dim only the sky contribution at
    // night. Each of the four cells touching the corner contributes; an opaque
    // cell is replaced by the centre's light, pulling the corner toward the
    // face's own light.
    let level_at = |p: [i32; 3]| -> (f32, f32) {
        let (block_light, sky_light) = neighbor_light(ctx, p[0], p[1], p[2]);
        (sky_light as f32, block_light as f32)
    };
    let avg = |get: &dyn Fn((f32, f32)) -> f32| -> f32 {
        let center = get(level_at(front));
        let l1 = if o1 { center } else { get(level_at(side1)) };
        let l2 = if o2 { center } else { get(level_at(side2)) };
        let lc = if oc { center } else { get(level_at(diag)) };
        (center + l1 + l2 + lc) * 0.25
    };
    let sky = light_curve(avg(&|(s, _)| s));
    let block = light_curve(avg(&|(_, b)| b));

    // Ambient occlusion, matching vanilla: the mean of the three neighbours'
    // occlusion value (1.0 open, 0.2 opaque) and the always-lit centre — i.e.
    // 1.0 − 0.2 × (opaque neighbours), giving 1.0 / 0.8 / 0.6 / 0.4.
    let occ = |o: bool| if o { 0.2 } else { 1.0 };
    let ao = (occ(o1) + occ(o2) + occ(oc) + 1.0) * 0.25;

    (sky, block, ao)
}

#[cfg(test)]
mod tests {
    use recraft_core::{BlockState, ChunkPos, SectionPos, World};

    use super::*;

    fn atlas() -> AtlasUv {
        crate::TextureAtlasImage::load_default().uv_table()
    }

    fn vpos(v: &ChunkVertex) -> [f32; 3] {
        [
            v.pos_light[0] as f32 / 64.0,
            v.pos_light[1] as f32 / 64.0,
            v.pos_light[2] as f32 / 64.0,
        ]
    }

    #[test]
    fn section_mesh_only_covers_its_own_section() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        // The block lives in section 0; section 0 has its six faces, every other
        // section is empty.
        let s0 = build_section_mesh(&world, SectionPos::new(0, 0, 0), &atlas(), BiomeColors::default(), false);
        assert_eq!(s0.solid.indices.len(), 36);
        let s1 = build_section_mesh(&world, SectionPos::new(0, 1, 0), &atlas(), BiomeColors::default(), false);
        assert!(s1.is_empty());
    }

    #[test]
    fn sections_sum_to_the_whole_column() {
        // A block in each of two different sections: the per-section meshes must
        // together equal the whole-column mesh.
        let mut world = World::new();
        world.set_block(1, 2, 3, BlockState::STONE); // section 0
        world.set_block(4, 40, 5, BlockState::STONE); // section 2
        let column = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        let mut total = 0;
        for sy in 0..16 {
            total += build_section_mesh(&world, SectionPos::new(0, sy, 0), &atlas(), BiomeColors::default(), false)
                .solid
                .indices
                .len();
        }
        assert_eq!(total, column.solid.indices.len());
        assert_eq!(total, 2 * 36);
    }

    #[test]
    fn vertical_border_culls_faces_across_sections() {
        // Two stacked blocks straddling the section-0/section-1 boundary (y=15 and
        // y=16). The shared faces must be culled even though the neighbour lives
        // in a different section, so each block keeps only five faces.
        let mut world = World::new();
        world.set_block(0, 15, 0, BlockState::STONE);
        world.set_block(0, 16, 0, BlockState::STONE);
        let s0 = build_section_mesh(&world, SectionPos::new(0, 0, 0), &atlas(), BiomeColors::default(), false);
        let s1 = build_section_mesh(&world, SectionPos::new(0, 1, 0), &atlas(), BiomeColors::default(), false);
        assert_eq!(s0.solid.indices.len(), 5 * 6, "y=15 block's top face culled");
        assert_eq!(s1.solid.indices.len(), 5 * 6, "y=16 block's bottom face culled");
    }

    #[test]
    fn neighborhood_section_path_matches_sync() {
        // The off-thread (snapshot) path and the synchronous (live World) path
        // must produce identical geometry for a section, including the vertical
        // border against the section below.
        let mut world = World::new();
        world.set_block(0, 15, 0, BlockState::STONE);
        world.set_block(0, 16, 0, BlockState::STONE);
        let sync = build_section_mesh(&world, SectionPos::new(0, 1, 0), &atlas(), BiomeColors::default(), false);
        let neighborhood = ChunkNeighborhood::snapshot(&world, ChunkPos::new(0, 0)).unwrap();
        let async_mesh = build_section_mesh_neighborhood(&neighborhood, 1, &atlas(), BiomeColors::default(), false);
        assert_eq!(sync.solid.vertices.len(), async_mesh.solid.vertices.len());
        assert_eq!(sync.solid.indices.len(), async_mesh.solid.indices.len());
        assert_eq!(async_mesh.solid.indices.len(), 5 * 6);
    }

    #[test]
    fn single_block_has_six_faces() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert_eq!(mesh.solid.vertices.len(), 24);
        assert_eq!(mesh.solid.indices.len(), 36);
    }

    #[test]
    fn smooth_lighting_is_flat_on_an_unoccluded_block() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        for quad in mesh.solid.vertices.chunks(4) {
            let c0 = quad[0].color;
            assert!(
                quad.iter().all(|v| v.color == c0),
                "an unoccluded face should be uniformly lit",
            );
        }
    }

    #[test]
    fn ambient_occlusion_darkens_occluded_top_corners() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        world.set_block(1, 1, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        let pos = |v: &ChunkVertex, axis: usize| v.pos_light[axis] as f32 / 64.0;
        let span = |q: &[ChunkVertex], axis: usize| {
            let lo = q.iter().map(|v| pos(v, axis)).fold(f32::MAX, f32::min);
            let hi = q.iter().map(|v| pos(v, axis)).fold(f32::MIN, f32::max);
            (lo, hi)
        };
        let top = mesh
            .solid
            .vertices
            .chunks(4)
            .find(|q| {
                q.iter().all(|v| (pos(v, 1) - 1.0).abs() < 0.02)
                    && span(q, 0) == (0.0, 1.0)
                    && span(q, 2) == (0.0, 1.0)
            })
            .expect("origin top face quad");
        let lum = |v: &ChunkVertex| v.color[0] as f32 / 255.0;
        let min = top.iter().map(lum).fold(f32::MAX, f32::min);
        let max = top.iter().map(lum).fold(f32::MIN, f32::max);
        assert!(
            max - min > 0.05,
            "ambient occlusion should darken the occluded corners (min {min}, max {max})",
        );
    }

    #[test]
    fn adjacent_blocks_cull_internal_faces() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        world.set_block(1, 0, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert_eq!(mesh.solid.indices.len(), 60);
    }

    #[test]
    fn leaves_go_to_cutout_buffer_and_stay_opaque() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(18, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert!(mesh.solid.is_empty());
        assert!(mesh.transparent.is_empty());
        assert_eq!(mesh.cutout.indices.len(), 36);
        // Cutout keeps full opacity (no global semi-transparency).
        assert!(mesh
            .cutout
            .vertices
            .iter()
            .all(|v| v.color[3] == 255));
    }

    #[test]
    fn adjacent_leaves_keep_internal_faces() {
        // Vanilla Fancy: two touching leaf blocks render all 12 faces (the
        // shared faces are NOT culled), unlike opaque or glass/ice blocks.
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(18, 0));
        world.set_block(1, 0, 0, BlockState::new(18, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert_eq!(mesh.cutout.indices.len(), 2 * 36, "leaves keep their shared faces");
    }

    #[test]
    fn tall_grass_renders_as_cross() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(31, 1));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        // Two planes, each emitted double-sided (front + back) for back-face
        // culling → 4 quads = 16 vertices, 24 indices, in the cutout pass.
        assert!(mesh.solid.is_empty());
        assert_eq!(mesh.cutout.vertices.len(), 16);
        assert_eq!(mesh.cutout.indices.len(), 24);
    }

    #[test]
    fn bottom_slab_is_half_height() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(44, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        let max_y = mesh
            .solid
            .vertices
            .iter()
            .map(|v| vpos(v)[1])
            .fold(f32::MIN, f32::max);
        assert!((max_y - 0.5).abs() < 0.02, "slab top was {max_y}");
    }

    #[test]
    fn previously_missing_block_states_resolve_textures() {
        // Every block state that used to log a missing-texture warning must
        // now either map all its faces to atlas tiles or render no geometry
        // at all (entity-rendered blocks like signs/banners/skulls).
        let uv = atlas();
        let states: &[(u16, u8)] = &[
            (146, 0), (33, 4), (90, 2), (51, 0), (145, 2), (145, 1), (145, 0),
            (154, 0), (131, 3), (100, 0), (145, 3), (118, 3), (177, 5), (148, 0),
            (92, 0), (140, 0), (144, 1), (154, 5), (154, 4), (54, 2), (147, 2),
            (147, 3), (33, 0), (120, 1), (143, 4), (138, 0), (96, 6), (96, 7),
            (96, 5), (96, 4), (148, 1), (54, 3), (54, 4), (54, 5), (143, 1),
            (72, 0), (70, 0),
        ];
        for &(id, meta) in states {
            let block = BlockState::new(id, meta);
            match block.render_shape() {
                RenderShape::None => continue,
                RenderShape::Boxes if block.render_boxes().as_slice().is_empty() => continue,
                _ => {}
            }
            for face in [BlockFace::Top, BlockFace::Bottom, BlockFace::Side] {
                let name = block.texture_name(face);
                assert!(
                    !uv.is_missing_tile(name),
                    "block {id}:{meta} {face:?} resolves to missing texture ({name:?})"
                );
            }
        }
    }

    #[test]
    fn barrier_renders_no_geometry() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(166, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert!(mesh.is_empty());
        assert_eq!(BlockState::new(166, 0).render_shape(), RenderShape::None);
        assert!(!BlockState::new(166, 0).is_opaque_cube());
    }

    #[test]
    fn stairs_render_base_plus_quarter() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(53, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert_eq!(mesh.solid.vertices.len(), 48);
        let max_y = mesh
            .solid
            .vertices
            .iter()
            .map(|v| vpos(v)[1])
            .fold(f32::MIN, f32::max);
        assert!((max_y - 1.0).abs() < 0.02, "stair top was {max_y}");
        assert!(mesh
            .solid
            .vertices
            .iter()
            .filter(|v| vpos(v)[1] > 0.75)
            .all(|v| vpos(v)[0] >= 0.5 - 0.02));
    }

    #[test]
    fn lone_fence_is_a_post_and_neighbours_grow_arm_bars() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(85, 0));
        let lone = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert_eq!(lone.solid.vertices.len(), 24, "post only");

        world.set_block(0, 0, -1, BlockState::STONE);
        let joined = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        // Post + two north bars (3 boxes) for the fence, 6 faces for the stone.
        let fence_vertices = joined.solid.vertices.len() as i32 - 24;
        assert_eq!(fence_vertices, 3 * 24);
    }

    #[test]
    fn lone_pane_is_a_cross_and_connections_drop_to_arms() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(102, 0));
        let lone = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        // Post + 4 arm panels.
        assert_eq!(lone.cutout.vertices.len(), 5 * 24);

        world.set_block(-1, 0, 0, BlockState::STONE);
        let joined = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        // Stone cube (5 visible faces x 4 = 20... full 6 since pane isn't opaque)
        // plus pane post + single west arm.
        let pane_vertices = joined.cutout.vertices.len();
        assert_eq!(pane_vertices, 2 * 24);
    }

    #[test]
    fn rails_rotate_and_slope() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(66, 0));
        world.set_block(1, 0, 0, BlockState::new(66, 2));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert_eq!(mesh.cutout.vertices.len(), 16, "one double-sided quad per rail");
        let max_y = mesh
            .cutout
            .vertices
            .iter()
            .map(|v| vpos(v)[1])
            .fold(f32::MIN, f32::max);
        assert!(
            (max_y - 1.0625).abs() < 0.02,
            "ascending rail top was {max_y}"
        );
    }

    #[test]
    fn cross_plants_are_inset_from_the_corners() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(38, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        for v in &mesh.cutout.vertices {
            let p = vpos(v);
            assert!(p[0] >= 0.05 - 0.02 && p[0] <= 0.95 + 0.02);
            assert!(p[2] >= 0.05 - 0.02 && p[2] <= 0.95 + 0.02);
        }
    }

    #[test]
    fn slab_side_faces_crop_the_texture_vertically() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(44, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        let uv = atlas();
        let rect = uv.tile_rect(BlockState::new(44, 0).texture_name(BlockFace::Side));
        let mut checked = 0;
        for quad in mesh.solid.vertices.chunks(4) {
            let constant = |axis: usize| {
                let p0 = vpos(&quad[0])[axis];
                quad.iter().all(|v| (vpos(v)[axis] - p0).abs() < 0.02)
            };
            if constant(1) {
                continue;
            }
            assert!(constant(0) || constant(2), "unexpected slanted quad");
            for v in quad {
                let v_coord = v.uv[1] as f32 / 65535.0;
                assert!(
                    v_coord >= rect[1] + 0.5 * rect[3] - 0.001,
                    "slab side sampled the upper texture half (v {v_coord})",
                );
            }
            checked += 1;
        }
        assert_eq!(checked, 4, "expected four side faces");
    }

    #[test]
    fn closed_door_is_a_thin_panel_on_the_facing_edge() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(64, 1));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert_eq!(mesh.cutout.vertices.len(), 24);
        let max_z = mesh
            .cutout
            .vertices
            .iter()
            .map(|v| vpos(v)[2])
            .fold(f32::MIN, f32::max);
        assert!((max_z - 0.1875).abs() < 0.02, "panel z extent was {max_z}");
    }

    #[test]
    fn opening_a_door_swings_the_panel_to_an_adjacent_edge() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(64, 1 | 4));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        let min_x = mesh
            .cutout
            .vertices
            .iter()
            .map(|v| vpos(v)[0])
            .fold(f32::MAX, f32::min);
        assert!((min_x - 0.8125).abs() < 0.02, "open panel min x was {min_x}");
    }

    #[test]
    fn piston_is_a_full_cube_and_head_is_plate_plus_arm() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(33, 1));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert_eq!(mesh.solid.vertices.len(), 24, "piston body is a full cube");

        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(34, 1));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false);
        assert_eq!(mesh.solid.vertices.len(), 48);
        let max_y = mesh
            .solid
            .vertices
            .iter()
            .map(|v| vpos(v)[1])
            .fold(f32::MIN, f32::max);
        assert!((max_y - 1.0).abs() < 0.02);
        let arm_min_x = mesh
            .solid
            .vertices
            .iter()
            .filter(|v| vpos(v)[1] < 0.75)
            .map(|v| vpos(v)[0])
            .fold(f32::MAX, f32::min);
        assert!((arm_min_x - 0.375).abs() < 0.02, "arm min x was {arm_min_x}");
    }

    #[test]
    fn door_and_piston_states_resolve_textures() {
        let uv = atlas();
        // Door upper/lower halves resolve to the matching texture.
        for &(id, meta) in &[(64, 0), (64, 8), (71, 0), (71, 8), (193, 8), (197, 0)] {
            let block = BlockState::new(id, meta);
            let face = if meta & 8 != 0 {
                BlockFace::Top
            } else {
                BlockFace::Bottom
            };
            assert!(
                !uv.is_missing_tile(block.texture_name(face)),
                "door {id}:{meta} resolves a tile"
            );
        }
        // Piston bodies + extended head resolve every face, and the inner face
        // the extended body needs is registered in the atlas.
        for &(id, meta) in &[(33, 1), (29, 9), (34, 1), (34, 9)] {
            let block = BlockState::new(id, meta);
            for face in [BlockFace::Top, BlockFace::Bottom, BlockFace::Side] {
                assert!(
                    !uv.is_missing_tile(block.texture_name(face)),
                    "piston {id}:{meta} {face:?} resolves a tile"
                );
            }
        }
        assert!(!uv.is_missing_tile(Some("piston_inner")));
    }
}
