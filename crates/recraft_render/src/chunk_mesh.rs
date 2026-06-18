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

    /// Same 28-byte storage, reinterpreted for the greedy (flat-lighting) pipeline:
    /// the `uv` slot holds the per-block REPEAT uv divided by [`GREEDY_UV_SCALE`]
    /// (so 0..16 fits Unorm16) and the `normal` slot holds the tile's atlas origin
    /// (2×u16 unorm). The greedy shader multiplies back by GREEDY_UV_SCALE and wraps
    /// `fract(repeat_uv)` within the tile, so a merged multi-block quad tiles its
    /// texture instead of stretching. Bytes are written by `encode_greedy_vertex`.
    pub const GREEDY_ATTRIBUTES: [wgpu::VertexAttribute; 4] =
        wgpu::vertex_attr_array![0 => Sint32x4, 1 => Unorm8x4, 2 => Unorm16x2, 3 => Unorm16x2];

    pub fn greedy_layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::GREEDY_ATTRIBUTES,
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
    flat: bool,
) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    for chunk in world.chunks() {
        append_chunk_mesh(world, chunk, &mut mesh, atlas, biome, fast_leaves, flat);
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
    flat: bool,
) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    if let Some(chunk) = world.chunk(pos.chunk()) {
        append_section_mesh(world, chunk, pos.y, &mut mesh, atlas, biome, fast_leaves, flat);
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
    flat: bool,
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
        flat,
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
    flat: bool,
) {
    for section in chunk.sections() {
        append_section_mesh(source, chunk, section.y(), mesh, atlas, biome, fast_leaves, flat);
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
    flat: bool,
) {
    let Some(section) = chunk.section(section_y) else {
        return;
    };
    let base_x = chunk.position.x * 16;
    let base_z = chunk.position.z * 16;
    let base_y = section_y * 16;
    // Flat (greedy) path: merge full-cube faces into big quads with per-face
    // lighting; non-cube shapes still go per-block below (flat-encoded) so slabs,
    // stairs, plants, torches etc. still render — they just don't merge.
    if flat {
        greedy_cube_mesh(source, base_x, base_y, base_z, atlas, biome, fast_leaves, mesh);
    }
    for y in 0..16i32 {
        for z in 0..16i32 {
            for x in 0..16i32 {
                let block = section.get(x as u8, y as u8, z as u8);
                if block.is_air() {
                    continue;
                }
                // Cubes are handled by greedy_cube_mesh in flat mode.
                if flat && block.render_shape() == RenderShape::Cube {
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
                    flat,
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
    /// Flat (greedy) mode: non-cube shapes emit greedy-format vertices (per-block
    /// repeat-UV + tile origin, flat light) so the greedy pipeline can draw them.
    flat: bool,
}

impl<S: BlockSource> BlockCtx<'_, S> {
    /// Resolve a face tint: a `setBlockTint` override wins, else the block's
    /// vanilla biome/constant tint.
    fn tint(&self, block: BlockState, face: BlockFace) -> [f32; 3] {
        block_tint(block).unwrap_or_else(|| tint_color(block.tint(face), self.biome))
    }
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
        RenderShape::Torch => append_torch(mesh, ctx, x, y, z, block),
        RenderShape::Fluid => append_fluid(mesh, ctx, x, y, z, block),
        RenderShape::Fire => append_fire(mesh, ctx, x, y, z, block),
        RenderShape::Bed => append_bed(mesh, ctx, x, y, z, block),
    }
}

/// Bed (id 26): a 9/16-tall directed block with two halves (foot / head).
///
/// Meta bits 0–1 encode facing (0=south, 1=west, 2=north, 3=east — the
/// direction the HEAD faces). Bit 3 (0x8) selects the head half.
///
/// Geometry (from vanilla `bed_foot.json` / `bed_head.json`):
/// - Main element: y 0..9/16, all four vertical sides rendered except the seam
///   joining the two halves. The top face uses a facing-dependent 90° UV
///   rotation (vanilla `"uv": [0,16,16,0], "rotation": 90`).
/// - Planks underside: flat DOWN face at y=3/16 (oak planks texture).
fn append_bed<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let is_head = block.meta & 8 != 0;
    let facing = block.meta & 3; // 0=south, 1=west, 2=north, 3=east

    let top_tex: Option<&str> = Some(if is_head { "bed_head_top" } else { "bed_feet_top" });
    let end_tex: Option<&str> = Some(if is_head { "bed_head_end" } else { "bed_feet_end" });
    let side_tex: Option<&str> = Some(if is_head { "bed_head_side" } else { "bed_feet_side" });

    // Facing direction vector (foot → head).
    let facing_dir: [i32; 3] = match facing {
        0 => [0, 0, 1],   // south
        1 => [-1, 0, 0],  // west
        2 => [0, 0, -1],  // north
        _ => [1, 0, 0],   // east
    };

    // Foot block: end face is opposite to facing; head block: end = facing.
    let end_normal: [i32; 3] = if is_head {
        facing_dir
    } else {
        [-facing_dir[0], 0, -facing_dir[2]]
    };
    // The open seam side (faces the other bed half) is not rendered.
    let open_normal: [i32; 3] = [-end_normal[0], 0, -end_normal[2]];

    const TOP: f32 = 9.0 / 16.0; // vanilla setBedBounds height (0.5625)
    let mn = [0.0f32, 0.0, 0.0];
    let mx = [1.0f32, TOP, 1.0];
    let alpha = 1.0;
    let buffer = buffer_for(mesh, block);

    // Vertical faces: the end face and the two sides perpendicular to facing.
    for face in &FACES {
        let n = face.normal;
        if n[1] != 0 { continue; }       // top/bottom handled separately
        if n == open_normal { continue; } // open seam side — not rendered
        let texture = if n == end_normal { end_tex } else { side_tex };
        let (nx, ny, nz) = (x + n[0], y + n[1], z + n[2]);
        emit_face(buffer, ctx, face, x, y, z, mn, mx, texture, block, alpha, nx, ny, nz);
    }

    // Top face with facing-dependent 90° UV rotation.
    {
        let face = &FACES[2]; // UP face ([0,1,0])
        warn_if_missing(ctx.atlas, block, "its top face", top_tex);
        let rect = ctx.atlas.tile_rect(top_tex);
        let front = [x, y + 1, z];
        let mut corners = [[0.0f32; 3]; 4];
        let mut uvs = [[0.0f32; 2]; 4];
        let mut colors = [[0.0f32; 4]; 4];
        let mut lights = [[0.0f32; 2]; 4];
        let mut local = [[0.0f32; 2]; 4];
        for (i, corner) in face.corners.iter().enumerate() {
            let px = corner[0];
            let pz = corner[2];
            corners[i] = [x as f32 + px, y as f32 + TOP, z as f32 + pz];
            let (u, v) = bed_top_uv(facing, px, pz);
            local[i] = [u, v];
            uvs[i] = rect_uv(rect, u, v);
            if ctx.flat { continue; }
            let (sky, blk, ao) = vertex_light(ctx, face.normal, front, *corner);
            colors[i] = [face.light * ao, face.light * ao, face.light * ao, alpha];
            lights[i] = [sky, blk];
        }
        if ctx.flat {
            let (blk_l, sky_l) = ctx.source.light_at(front[0], front[1], front[2]);
            let light = [sky_l as f32 / 15.0, blk_l as f32 / 15.0];
            let color = [face.light, face.light, face.light, alpha];
            push_greedy_quad(buffer, corners, local, [rect[0], rect[1]], color, light);
        } else {
            buffer.push_quad_smooth(corners, uvs, colors, lights);
        }
    }

    // Planks underside (vanilla element 2): flat DOWN face at y=3/16.
    {
        let face = &FACES[3]; // DOWN face ([0,-1,0])
        let pmn = [0.0f32, 3.0 / 16.0, 0.0];
        let pmx = [1.0f32, 3.0 / 16.0, 1.0];
        emit_face(buffer, ctx, face, x, y, z, pmn, pmx, Some("planks_oak"), block, alpha, x, y - 1, z);
    }
}

/// UV mapping for the bed's top face. The vanilla model uses `"uv": [0,16,16,0],
/// "rotation": 90`, then the blockstate applies a Y-rotation per facing direction.
/// Composed, this maps (px, pz) ∈ {0,1}² to the following (u, v) in tile space:
fn bed_top_uv(facing: u8, px: f32, pz: f32) -> (f32, f32) {
    match facing {
        0 => (1.0 - pz, px),        // south (model y=0)
        1 => (1.0 - px, 1.0 - pz),  // west  (model y=90)
        2 => (pz, 1.0 - px),        // north (model y=180)
        _ => (px, pz),              // east  (model y=270)
    }
}

/// Fire (`BlockFire`): tall crossed diagonal planes on the floor, plus a plane
/// clinging to each adjacent solid wall (fire climbing it). 1.4 blocks tall,
/// double-sided, full-bright. Frame animation (fire_layer_0/1 flicker) needs a
/// texture-atlas animation clock the chunk shader doesn't have yet — deferred,
/// so this renders a static (but correctly shaped, wall-aware) flame ⚠️.
fn append_fire<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let texture = block.texture_name(BlockFace::Side);
    warn_if_missing(ctx.atlas, block, "its fire texture", texture);
    let rect = ctx.atlas.tile_rect(texture);
    // Corner order is [ground-a, ground-b, top-b, top-a]; map the ground edge to
    // the texture bottom (v + h) and the top edge to the texture top (v) so the
    // flame stands upright instead of upside-down.
    let uv = inset_tile_uvs(
        [
            [rect[0], rect[1] + rect[3]],
            [rect[0] + rect[2], rect[1] + rect[3]],
            [rect[0] + rect[2], rect[1]],
            [rect[0], rect[1]],
        ],
        ctx.atlas,
    );
    // Fire emits light; render near full-bright using its own cell light.
    let light = face_light(ctx, x, y, z);
    let color = [1.0, 1.0, 1.0, 1.0];
    let (fx, fy, fz) = (x as f32, y as f32, z as f32);
    const H: f32 = 1.4; // vanilla fire is 22.4px ≈ 1.4 blocks tall

    let quad = |a: [f32; 3], b: [f32; 3]| {
        // A vertical quad from ground edge (a→b) up to height H.
        [
            [fx + a[0], fy, fz + a[2]],
            [fx + b[0], fy, fz + b[2]],
            [fx + b[0], fy + H, fz + b[2]],
            [fx + a[0], fy + H, fz + a[2]],
        ]
    };

    let opaque = |dx: i32, dy: i32, dz: i32| neighbor_block(ctx, x + dx, y + dy, z + dz).is_opaque_cube();

    // Planes clinging to adjacent solid walls.
    let mut any_wall = false;
    if opaque(-1, 0, 0) {
        any_wall = true;
        emit_double_sided(mesh, ctx, block, quad([0.05, 0.0, 0.0], [0.05, 0.0, 1.0]), uv, color, light);
    }
    if opaque(1, 0, 0) {
        any_wall = true;
        emit_double_sided(mesh, ctx, block, quad([0.95, 0.0, 1.0], [0.95, 0.0, 0.0]), uv, color, light);
    }
    if opaque(0, 0, -1) {
        any_wall = true;
        emit_double_sided(mesh, ctx, block, quad([1.0, 0.0, 0.05], [0.0, 0.0, 0.05]), uv, color, light);
    }
    if opaque(0, 0, 1) {
        any_wall = true;
        emit_double_sided(mesh, ctx, block, quad([0.0, 0.0, 0.95], [1.0, 0.0, 0.95]), uv, color, light);
    }

    // Floor fire (crossed diagonal planes) when sitting on a solid block or when
    // there's no wall to cling to (so airborne fire still shows something).
    if opaque(0, -1, 0) || !any_wall {
        emit_double_sided(mesh, ctx, block, quad([0.1, 0.0, 0.1], [0.9, 0.0, 0.9]), uv, color, light);
        emit_double_sided(mesh, ctx, block, quad([0.9, 0.0, 0.1], [0.1, 0.0, 0.9]), uv, color, light);
    }
}

/// Vanilla `BlockLiquid` surface height for a fluid level (meta). Source (0) and
/// falling (>=8) sit near the top; flowing levels 1-7 step down. (`1 -
/// (level+1)/9`, the `getLiquidHeightPercent` shape, flattened per-block — the
/// smooth four-corner slope from `BlockFluidRenderer.getFluidHeight` is a
/// follow-up; flow-direction UV rotation is likewise omitted ⚠️.)
fn fluid_surface_height(meta: u8) -> f32 {
    let level = if meta >= 8 { 0 } else { meta };
    1.0 - (level as f32 + 1.0) / 9.0
}

/// Water/lava (`BlockLiquid`): a box whose top sits at the level-derived surface
/// height, so flowing fluid renders as the stepped "incomplete" blocks rather
/// than full cubes. Faces cull against opaque neighbours; a same-fluid neighbour
/// hides the shared part (a taller block still shows its exposed upper strip).
fn append_fluid<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let same_fluid = |b: BlockState| {
        (block.is_water() && b.is_water()) || (block.is_lava() && b.is_lava())
    };
    // A continuous column (same fluid above) renders full height.
    let above = neighbor_block(ctx, x, y + 1, z);
    let height = if same_fluid(above) {
        1.0
    } else {
        fluid_surface_height(block.meta)
    };
    let alpha = block.render_alpha();
    let buffer = buffer_for(mesh, block);
    for face in &FACES {
        let (nx, ny, nz) = (x + face.normal[0], y + face.normal[1], z + face.normal[2]);
        let neighbor = neighbor_block(ctx, nx, ny, nz);
        if neighbor.is_opaque_cube() {
            continue;
        }
        let mut mn = [0.0f32; 3];
        let mx = [1.0f32, height, 1.0f32];
        match face.normal {
            [0, 1, 0] => {
                // Top surface — hidden when the column continues upward.
                if same_fluid(above) {
                    continue;
                }
            }
            [0, -1, 0] => {
                // Bottom — hidden when fluid continues below.
                if same_fluid(neighbor) {
                    continue;
                }
            }
            _ => {
                if same_fluid(neighbor) {
                    let n_above = neighbor_block(ctx, nx, ny + 1, nz);
                    let n_height = if same_fluid(n_above) {
                        1.0
                    } else {
                        fluid_surface_height(neighbor.meta)
                    };
                    if n_height >= height {
                        continue; // fully hidden by an equal/taller neighbour
                    }
                    mn[1] = n_height; // only the exposed strip above the neighbour
                }
            }
        }
        emit_face(
            buffer,
            ctx,
            face,
            x,
            y,
            z,
            mn,
            mx,
            block.texture_name(face.face),
            block,
            alpha,
            nx,
            ny,
            nz,
        );
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
    let tint = ctx.tint(block, face.face);
    warn_if_missing(ctx.atlas, block, face_context(face.face), texture);
    let rect = ctx.atlas.tile_rect(texture);
    let front = [nx, ny, nz];
    let mut corners = [[0.0f32; 3]; 4];
    let mut uvs = [[0.0f32; 2]; 4];
    let mut colors = [[0.0f32; 4]; 4];
    let mut lights = [[0.0f32; 2]; 4];
    let mut local = [[0.0f32; 2]; 4];
    for (i, corner) in face.corners.iter().enumerate() {
        let px = lerp_axis(corner[0], mn[0], mx[0]);
        let py = lerp_axis(corner[1], mn[1], mx[1]);
        let pz = lerp_axis(corner[2], mn[2], mx[2]);
        corners[i] = [x as f32 + px, y as f32 + py, z as f32 + pz];
        let (u, v) = face_uv(face.normal, px, py, pz);
        local[i] = [u, v];
        uvs[i] = rect_uv(rect, u, v);
        // In flat mode the per-vertex smooth light below is unused (we shade flat).
        if ctx.flat {
            continue;
        }
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
    if ctx.flat {
        // Flat per-face light from the exposed neighbour (matches greedy cubes),
        // greedy-encoded so the greedy pipeline draws it. Repeat-UV is the local
        // face UV (≤1), tile origin from the tile rect.
        let (block_l, sky_l) = ctx.source.light_at(nx, ny, nz);
        let light = [sky_l as f32 / 15.0, block_l as f32 / 15.0];
        let color = [tint[0] * face.light, tint[1] * face.light, tint[2] * face.light, alpha];
        push_greedy_quad(buffer, corners, local, [rect[0], rect[1]], color, light);
    } else {
        buffer.push_quad_smooth(corners, uvs, colors, lights);
    }
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

/// Vanilla per-position render offset for plants that declare an `OffsetType`:
/// flowers and double plants (`XZ`) and tall grass/fern (`XYZ`). A deterministic
/// hash of the block's world X/Z nudges the model up to ±0.25 horizontally (and
/// tall grass 0..0.2 downward) so dense plant cover doesn't look gridded; every
/// other plant stays centred. Mirrors `BlockModelRenderer.renderModelStandardQuads`
/// — the `cross` model disables ambient occlusion, so vanilla always takes that
/// y-independent hash path, which is also why a double plant's two halves line up.
fn cross_plant_offset(id: u16, x: i32, z: i32) -> (f32, f32, f32) {
    let xyz = match id {
        31 => true,             // tall grass / fern
        37 | 38 | 175 => false, // dandelion / small flowers / double plant
        _ => return (0.0, 0.0, 0.0),
    };
    let mut k = (x.wrapping_mul(3129871) as i64) ^ (z as i64).wrapping_mul(116129781);
    k = k.wrapping_mul(k).wrapping_mul(42317861).wrapping_add(k.wrapping_mul(11));
    let ox = (((k >> 16) & 15) as f32 / 15.0 - 0.5) * 0.5;
    let oz = (((k >> 24) & 15) as f32 / 15.0 - 0.5) * 0.5;
    let oy = if xyz {
        (((k >> 20) & 15) as f32 / 15.0 - 1.0) * 0.2
    } else {
        0.0
    };
    (ox, oy, oz)
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
    let tint = ctx.tint(block, BlockFace::Side);
    let color = [tint[0], tint[1], tint[2], block.render_alpha()];
    let texture = block.texture_name(BlockFace::Side);
    warn_if_missing(ctx.atlas, block, "its cross texture", texture);
    let uvs = inset_tile_uvs(ctx.atlas.uv(texture), ctx.atlas);
    let (ox, oy, oz) = cross_plant_offset(block.id, x, z);
    let (fx, fy, fz) = (x as f32 + ox, y as f32 + oy, z as f32 + oz);
    // Vanilla cross model: the diagonals are inset 0.8/16 from the corners
    // (rotated 45° with rescale), not stretched corner-to-corner.
    let lo = 0.05;
    let hi = 0.95;
    // Corner order matches the UV table convention (bottom, top, top, bottom)
    // so the texture stands upright instead of twisting across the quad.
    let q0 = [
        [fx + lo, fy, fz + lo],
        [fx + lo, fy + 1.0, fz + lo],
        [fx + hi, fy + 1.0, fz + hi],
        [fx + hi, fy, fz + hi],
    ];
    let q1 = [
        [fx + hi, fy, fz + lo],
        [fx + hi, fy + 1.0, fz + lo],
        [fx + lo, fy + 1.0, fz + hi],
        [fx + lo, fy, fz + hi],
    ];
    let buffer = buffer_for(mesh, block);
    if ctx.flat {
        let (ruv, origin) = abs_uvs_to_repeat(uvs, ctx.atlas);
        push_greedy_quad_double_sided(buffer, q0, ruv, origin, color, light);
        push_greedy_quad_double_sided(buffer, q1, ruv, origin, color, light);
    } else {
        buffer.push_quad_double_sided(q0, uvs, color, light);
        buffer.push_quad_double_sided(q1, uvs, color, light);
    }
}

/// Pull a full-tile UV quad in by half a texel on every side. With nearest
/// filtering the raw tile bounds land exactly on the texel grid, so a fragment
/// right at the quad edge floors into the neighbouring atlas tile — and since the
/// quad carries a vertex tint (grass/foliage green, etc.), any opaque neighbour
/// texel that bleeds in shows up as a shimmering bright speck along the edges.
/// Sampling texel centres instead keeps every fetch inside the sprite. Used by
/// the full-tile shapes (cross plants, ladders).
fn inset_tile_uvs(uvs: [[f32; 2]; 4], atlas: &AtlasUv) -> [[f32; 2]; 4] {
    let [du, dv] = atlas.tile_size();
    let (hu, hv) = (du / 32.0, dv / 32.0); // half a texel (a tile is 16 texels)
    let cu = (uvs[0][0] + uvs[2][0]) * 0.5;
    let cv = (uvs[0][1] + uvs[1][1]) * 0.5;
    let pull = |c: [f32; 2]| {
        [
            if c[0] < cu { c[0] + hu } else { c[0] - hu },
            if c[1] < cv { c[1] + hv } else { c[1] - hv },
        ]
    };
    [pull(uvs[0]), pull(uvs[1]), pull(uvs[2]), pull(uvs[3])]
}

/// Map an in-tile `(u, v)` fraction (0..1 over the box face) to an atlas UV,
/// clamped half a texel inside the tile rect. Same fix as [`inset_tile_uvs`] but
/// for the box/cube path, where faces spanning the full extent land on the tile
/// edge and would otherwise floor into a neighbour (visible on cutout/translucent
/// blocks: glass, stained glass, panes, iron bars, leaves). Interior crops
/// (slabs, stairs) are untouched — only the 0/1 extremes get pulled in.
fn rect_uv(rect: [f32; 4], u: f32, v: f32) -> [f32; 2] {
    let (hu, hv) = (rect[2] / 32.0, rect[3] / 32.0); // half a texel
    [
        (rect[0] + u * rect[2]).clamp(rect[0] + hu, rect[0] + rect[2] - hu),
        (rect[1] + v * rect[3]).clamp(rect[1] + hv, rect[1] + rect[3] - hv),
    ]
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
    let tint = ctx.tint(block, BlockFace::Top);
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
    let mut uvs = inset_tile_uvs(
        [
            [rect[0], rect[1]],
            [rect[0] + rect[2], rect[1]],
            [rect[0] + rect[2], rect[1] + rect[3]],
            [rect[0], rect[1] + rect[3]],
        ],
        ctx.atlas,
    );
    uvs.rotate_right(turns);
    // Double-sided so the track is visible from above and (through a glass
    // floor) below, under back-face culling.
    emit_double_sided(mesh, ctx, block, corners, uvs, color, light);
}

/// Torch / redstone torch (`BlockTorch`): a thin 2px post with the torch
/// texture's centre column on its sides and the flame on its top. Floor torches
/// (meta 0/5) stand vertical and centred; wall torches lean outward from the
/// mounting wall chosen by meta 1-4 (EAST/WEST/SOUTH/NORTH). The lean is a
/// faithful approximation of vanilla `renderTorchAtAngle` (exact angle ⚠️
/// pixel-level); the position/wall side and the centre-column UV are exact.
fn append_torch<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let texture = block.texture_name(BlockFace::Side);
    warn_if_missing(ctx.atlas, block, "its torch texture", texture);
    let rect = ctx.atlas.tile_rect(texture);
    // Sub-tile UV by pixel (16px tile): u for the centre column 7..9, v top→down.
    // Clamp half a texel inside the tile so the full-height v 0/16 edges don't
    // floor into the neighbouring atlas tile under nearest sampling (see rect_uv).
    let (hu, hv) = (rect[2] / 32.0, rect[3] / 32.0);
    let u = |px: f32| (rect[0] + (px / 16.0) * rect[2]).clamp(rect[0] + hu, rect[0] + rect[2] - hu);
    let v = |px: f32| (rect[1] + (px / 16.0) * rect[3]).clamp(rect[1] + hv, rect[1] + rect[3] - hv);

    // Torches are self-lit; render near full-bright (block-light 14 ≈ sky-less).
    let light = face_light(ctx, x, y + 1, z);
    let color = [1.0, 1.0, 1.0, block.render_alpha()];

    // Bottom- and top-centre of the post in unit block space. Wall torches sit
    // low against the wall and lean toward the cell centre as they rise.
    let (bc, tc): ([f32; 3], [f32; 3]) = match block.meta & 7 {
        1 => ([0.1, 0.2, 0.5], [0.5, 0.8, 0.5]), // EAST  → on -X wall, lean +x
        2 => ([0.9, 0.2, 0.5], [0.5, 0.8, 0.5]), // WEST  → on +X wall, lean -x
        3 => ([0.5, 0.2, 0.1], [0.5, 0.8, 0.5]), // SOUTH → on -Z wall, lean +z
        4 => ([0.5, 0.2, 0.9], [0.5, 0.8, 0.5]), // NORTH → on +Z wall, lean -z
        _ => ([0.5, 0.0, 0.5], [0.5, 0.625, 0.5]), // floor: vertical, 10/16 tall
    };
    let (fx, fy, fz) = (x as f32, y as f32, z as f32);
    let hw = 1.0 / 16.0; // half the 2px post width
    // Bottom and top square corners (NW, NE, SE, SW) around the centres.
    let square = |c: [f32; 3]| {
        [
            [fx + c[0] - hw, fy + c[1], fz + c[2] - hw],
            [fx + c[0] + hw, fy + c[1], fz + c[2] - hw],
            [fx + c[0] + hw, fy + c[1], fz + c[2] + hw],
            [fx + c[0] - hw, fy + c[1], fz + c[2] + hw],
        ]
    };
    let b = square(bc);
    let t = square(tc);

    // Side faces sample the centre column (u 7..9), full height (v 0..16).
    let side_uv = [[u(9.0), v(0.0)], [u(7.0), v(0.0)], [u(7.0), v(16.0)], [u(9.0), v(16.0)]];
    // Four side quads (top edge from `t`, bottom edge from `b`), each wound CCW.
    let sides = [
        [t[0], t[3], b[3], b[0]], // -X
        [t[2], t[1], b[1], b[2]], // +X
        [t[1], t[0], b[0], b[1]], // -Z
        [t[3], t[2], b[2], b[3]], // +Z
    ];
    for corners in sides {
        emit_double_sided(mesh, ctx, block, corners, side_uv, color, light);
    }
    // Top face: the flame nub (u 7..9, v 6..8).
    let top_uv = [[u(7.0), v(6.0)], [u(9.0), v(6.0)], [u(9.0), v(8.0)], [u(7.0), v(8.0)]];
    emit_double_sided(mesh, ctx, block, [t[0], t[1], t[2], t[3]], top_uv, color, light);
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
    let tint = ctx.tint(block, BlockFace::Side);
    let color = [tint[0], tint[1], tint[2], block.render_alpha()];
    let texture = block.texture_name(BlockFace::Side);
    warn_if_missing(ctx.atlas, block, "its ladder texture", texture);
    let uvs = inset_tile_uvs(ctx.atlas.uv(texture), ctx.atlas);
    // Double-sided so the rungs show under back-face culling regardless of which
    // way the single quad happens to wind.
    emit_double_sided(mesh, ctx, block, corners, uvs, color, light);
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

/// Static per-block tint overrides populated by the extension preset
/// `setBlockTint`. Keyed by block id, optionally narrowed to one meta.
#[derive(Debug, Clone, Default)]
pub struct TintTable {
    by_id: std::collections::HashMap<u16, [f32; 3]>,
    by_id_meta: std::collections::HashMap<(u16, u8), [f32; 3]>,
}

impl TintTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// `meta = None` tints every meta of `id`; `Some(m)` narrows to one meta and
    /// takes precedence over an all-meta entry.
    pub fn set(&mut self, id: u16, meta: Option<u8>, color: [f32; 3]) {
        match meta {
            Some(m) => {
                self.by_id_meta.insert((id, m), color);
            }
            None => {
                self.by_id.insert(id, color);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.by_id_meta.is_empty()
    }

    fn lookup(&self, block: BlockState) -> Option<[f32; 3]> {
        self.by_id_meta
            .get(&(block.id, block.meta))
            .or_else(|| self.by_id.get(&block.id))
            .copied()
    }
}

static TINTS: std::sync::LazyLock<std::sync::RwLock<TintTable>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(TintTable::new()));
static HAS_TINTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Install the process-wide block tint overrides (from `setBlockTint`). Meshing
/// reads them via [`block_tint`] with an atomic fast-path, so the common
/// no-override case costs one relaxed load per face.
pub fn set_block_tints(table: TintTable) {
    let empty = table.is_empty();
    *TINTS.write().unwrap() = table;
    HAS_TINTS.store(!empty, std::sync::atomic::Ordering::Relaxed);
}

/// The tint override for `block`, if any (read by the mesher + worker threads).
#[inline]
pub fn block_tint(block: BlockState) -> Option<[f32; 3]> {
    if !HAS_TINTS.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    TINTS.read().unwrap().lookup(block)
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

/// Max merge extent in blocks (a section is 16³); repeat-UV is divided by this to
/// fit Unorm16 and multiplied back in the greedy shader.
pub const GREEDY_UV_SCALE: f32 = 16.0;

/// Encode a greedy (flat-lighting) vertex into the shared `ChunkVertex` storage:
/// the `uv` slot carries the per-block repeat UV / `GREEDY_UV_SCALE`, the `normal`
/// slot carries the tile's atlas origin (2×u16 unorm). Read back by the greedy
/// vertex layout + shader.
fn encode_greedy_vertex(
    position: [f32; 3],
    color: [f32; 4],
    repeat_uv: [f32; 2],
    tile_origin: [f32; 2],
    light: [f32; 2],
) -> ChunkVertex {
    let px = (position[0] * 64.0).round() as i32;
    let py = (position[1] * 64.0).round() as i32;
    let pz = (position[2] * 64.0).round() as i32;
    let sky_u8 = (light[0] * 255.0 + 0.5) as u8;
    let block_u8 = (light[1] * 255.0 + 0.5) as u8;
    let w = ((sky_u8 as i32) << 8) | (block_u8 as i32);
    let unorm = |x: f32| (x.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
    let to: [u16; 2] = [unorm(tile_origin[0]), unorm(tile_origin[1])];
    ChunkVertex {
        pos_light: [px, py, pz, w],
        color: [
            (color[0] * 255.0 + 0.5) as u8,
            (color[1] * 255.0 + 0.5) as u8,
            (color[2] * 255.0 + 0.5) as u8,
            (color[3] * 255.0 + 0.5) as u8,
        ],
        uv: [
            unorm(repeat_uv[0] / GREEDY_UV_SCALE),
            unorm(repeat_uv[1] / GREEDY_UV_SCALE),
        ],
        // Reinterpret the 4-byte normal slot as 2×u16 (atlas tile origin).
        normal: bytemuck::cast(to),
    }
}

fn push_greedy_quad(
    buffer: &mut ChunkMeshBuffers,
    corners: [[f32; 3]; 4],
    repeat_uvs: [[f32; 2]; 4],
    tile_origin: [f32; 2],
    color: [f32; 4],
    light: [f32; 2],
) {
    let base = buffer.vertices.len() as u16;
    for i in 0..4 {
        buffer
            .vertices
            .push(encode_greedy_vertex(corners[i], color, repeat_uvs[i], tile_origin, light));
    }
    buffer
        .indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn push_greedy_quad_double_sided(
    buffer: &mut ChunkMeshBuffers,
    corners: [[f32; 3]; 4],
    repeat_uvs: [[f32; 2]; 4],
    tile_origin: [f32; 2],
    color: [f32; 4],
    light: [f32; 2],
) {
    push_greedy_quad(buffer, corners, repeat_uvs, tile_origin, color, light);
    let back = [corners[3], corners[2], corners[1], corners[0]];
    let back_uv = [repeat_uvs[3], repeat_uvs[2], repeat_uvs[1], repeat_uvs[0]];
    push_greedy_quad(buffer, back, back_uv, tile_origin, color, light);
}

/// Push a double-sided quad, choosing the greedy (flat) or smooth encoding by the
/// context — shared by the cross/rail/ladder shapes.
fn emit_double_sided<S: BlockSource>(
    mesh: &mut ChunkMesh,
    ctx: &BlockCtx<S>,
    block: BlockState,
    corners: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    color: [f32; 4],
    light: [f32; 2],
) {
    let buffer = buffer_for(mesh, block);
    if ctx.flat {
        let (ruv, origin) = abs_uvs_to_repeat(uvs, ctx.atlas);
        push_greedy_quad_double_sided(buffer, corners, ruv, origin, color, light);
    } else {
        buffer.push_quad_double_sided(corners, uvs, color, light);
    }
}

/// Convert absolute atlas UVs to greedy repeat-UVs (anchored at the tile origin)
/// for the flat per-block path of non-cube shapes. The shape's UVs all lie within
/// one tile, so the component-wise min is that tile's origin.
fn abs_uvs_to_repeat(uvs: [[f32; 2]; 4], atlas: &AtlasUv) -> ([[f32; 2]; 4], [f32; 2]) {
    let ts = atlas.tile_size();
    let origin = [
        uvs.iter().map(|u| u[0]).fold(f32::INFINITY, f32::min),
        uvs.iter().map(|u| u[1]).fold(f32::INFINITY, f32::min),
    ];
    let mut repeat = [[0.0f32; 2]; 4];
    for i in 0..4 {
        repeat[i] = [(uvs[i][0] - origin[0]) / ts[0], (uvs[i][1] - origin[1]) / ts[1]];
    }
    (repeat, origin)
}

/// Merge key: only identical blocks lit identically (same exposed-face skylight +
/// block light) on the same face merge into one quad.
#[derive(Clone, Copy, PartialEq, Eq)]
struct GreedyKey {
    block: BlockState,
    sky: u8,
    block_light: u8,
}

/// Greedy mesh of the full-cube blocks of one 16³ section: for each of the 6 face
/// directions, sweep the 16 slices, build a per-(u,v) mask of visible-face merge
/// keys, and merge maximal rectangles into single quads with flat (per-face)
/// lighting. Non-cube shapes (cross/rail/…) are left to the per-block path.
/// Reuses `face.corners` + `face_uv` for geometry/winding; only the UV becomes a
/// per-block repeat coordinate the greedy shader wraps within the tile.
pub fn greedy_cube_mesh<S: BlockSource>(
    source: &S,
    base_x: i32,
    base_y: i32,
    base_z: i32,
    atlas: &AtlasUv,
    biome: BiomeColors,
    fast_leaves: bool,
    mesh: &mut ChunkMesh,
) {
    for face in &FACES {
        let n_axis = face.normal.iter().position(|&c| c != 0).expect("axis-aligned");
        let n_sign = face.normal[n_axis];
        // The two in-plane world axes (a = outer/height, b = inner/width).
        let (a_axis, b_axis) = match n_axis {
            0 => (1usize, 2usize),
            1 => (0, 2),
            _ => (0, 1),
        };
        // Unit-corner texture coords (0/1) carry the face's UV orientation.
        let uv01: [[f32; 2]; 4] = {
            let mut o = [[0.0; 2]; 4];
            for (i, c) in face.corners.iter().enumerate() {
                let (u, v) = face_uv(face.normal, c[0], c[1], c[2]);
                o[i] = [u, v];
            }
            o
        };
        for slice in 0..16i32 {
            let mut mask: [Option<GreedyKey>; 256] = [None; 256];
            for a in 0..16i32 {
                for b in 0..16i32 {
                    let mut local = [0i32; 3];
                    local[n_axis] = slice;
                    local[a_axis] = a;
                    local[b_axis] = b;
                    let block =
                        source.block_at(base_x + local[0], base_y + local[1], base_z + local[2]);
                    if block.is_air() || block.render_shape() != RenderShape::Cube {
                        continue;
                    }
                    let mut nb = local;
                    nb[n_axis] += n_sign;
                    let neighbor =
                        source.block_at(base_x + nb[0], base_y + nb[1], base_z + nb[2]);
                    let merges = neighbor.id == block.id && (fast_leaves || !block.is_leaves());
                    if neighbor.is_opaque_cube() || merges {
                        continue;
                    }
                    // light_at returns (block_light, sky_light) — keep that order.
                    let (block_light, sky) =
                        source.light_at(base_x + nb[0], base_y + nb[1], base_z + nb[2]);
                    mask[(a * 16 + b) as usize] = Some(GreedyKey { block, sky, block_light });
                }
            }
            // Greedy maximal-rectangle merge over the mask.
            for a in 0..16i32 {
                let mut b = 0i32;
                while b < 16 {
                    let Some(key) = mask[(a * 16 + b) as usize] else {
                        b += 1;
                        continue;
                    };
                    let mut w = 1;
                    while b + w < 16 && mask[(a * 16 + b + w) as usize] == Some(key) {
                        w += 1;
                    }
                    let mut h = 1;
                    'grow: while a + h < 16 {
                        for k in 0..w {
                            if mask[((a + h) * 16 + b + k) as usize] != Some(key) {
                                break 'grow;
                            }
                        }
                        h += 1;
                    }
                    for da in 0..h {
                        for db in 0..w {
                            mask[((a + da) * 16 + b + db) as usize] = None;
                        }
                    }
                    emit_greedy_quad(
                        base_x, base_y, base_z, n_axis, a_axis, b_axis, slice, a, b, h, w, &key,
                        face, &uv01, atlas, biome, mesh,
                    );
                    b += w;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_greedy_quad(
    base_x: i32,
    base_y: i32,
    base_z: i32,
    n_axis: usize,
    a_axis: usize,
    b_axis: usize,
    slice: i32,
    a: i32,
    b: i32,
    h: i32, // extent along a_axis
    w: i32, // extent along b_axis
    key: &GreedyKey,
    face: &Face,
    uv01: &[[f32; 2]; 4],
    atlas: &AtlasUv,
    biome: BiomeColors,
    mesh: &mut ChunkMesh,
) {
    // Box in block units: 1 thick on the normal axis, h×w in plane.
    let mut mn = [0i32; 3];
    let mut ext = [1i32; 3];
    mn[n_axis] = slice;
    mn[a_axis] = a;
    mn[b_axis] = b;
    ext[a_axis] = h;
    ext[b_axis] = w;
    let cell = [base_x + mn[0], base_y + mn[1], base_z + mn[2]];
    // Repeat spans along the face's texture U and V (uv01 carries the orientation;
    // n_axis decides which in-plane extent maps to U vs V — see face_uv).
    let (span_u, span_v) = match n_axis {
        0 => (w as f32, h as f32),
        _ => (h as f32, w as f32),
    };
    let tint =
        block_tint(key.block).unwrap_or_else(|| tint_color(key.block.tint(face.face), biome));
    let shade = face.light;
    let color = [
        tint[0] * shade,
        tint[1] * shade,
        tint[2] * shade,
        key.block.render_alpha(),
    ];
    let light = [key.sky as f32 / 15.0, key.block_light as f32 / 15.0];
    let texture = key.block.texture_name(face.face);
    let rect = atlas.tile_rect(texture);
    let tile_origin = [rect[0], rect[1]];
    let mut corners = [[0.0f32; 3]; 4];
    let mut repeat = [[0.0f32; 2]; 4];
    for (i, c) in face.corners.iter().enumerate() {
        corners[i] = [
            cell[0] as f32 + c[0] * ext[0] as f32,
            cell[1] as f32 + c[1] * ext[1] as f32,
            cell[2] as f32 + c[2] * ext[2] as f32,
        ];
        repeat[i] = [uv01[i][0] * span_u, uv01[i][1] * span_v];
    }
    push_greedy_quad(buffer_for(mesh, key.block), corners, repeat, tile_origin, color, light);
}

#[cfg(test)]
mod tests {
    use recraft_core::{BlockState, ChunkPos, SectionPos, World};

    use super::*;

    fn atlas() -> AtlasUv {
        crate::TextureAtlasImage::load_default().uv_table()
    }

    #[test]
    fn tint_table_meta_overrides_id() {
        let mut t = TintTable::new();
        t.set(1, None, [0.1, 0.2, 0.3]);
        t.set(1, Some(5), [0.9, 0.8, 0.7]);
        assert_eq!(t.lookup(BlockState { id: 1, meta: 0 }), Some([0.1, 0.2, 0.3]));
        assert_eq!(t.lookup(BlockState { id: 1, meta: 5 }), Some([0.9, 0.8, 0.7]));
        assert_eq!(t.lookup(BlockState { id: 2, meta: 0 }), None);
    }

    #[test]
    fn global_block_tint_set_and_fast_path() {
        // id 200 is above MAX_BLOCK_ID, so no meshing test will ever look it up —
        // keeps this global-state test from racing concurrent meshing tests.
        assert_eq!(block_tint(BlockState { id: 200, meta: 0 }), None);
        let mut t = TintTable::new();
        t.set(200, None, [1.0, 0.0, 0.0]);
        set_block_tints(t);
        assert_eq!(block_tint(BlockState { id: 200, meta: 0 }), Some([1.0, 0.0, 0.0]));
        assert_eq!(block_tint(BlockState { id: 201, meta: 0 }), None);
        set_block_tints(TintTable::new()); // reset the global fast-path
        assert_eq!(block_tint(BlockState { id: 200, meta: 0 }), None);
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
        let s0 = build_section_mesh(&world, SectionPos::new(0, 0, 0), &atlas(), BiomeColors::default(), false, false);
        assert_eq!(s0.solid.indices.len(), 36);
        let s1 = build_section_mesh(&world, SectionPos::new(0, 1, 0), &atlas(), BiomeColors::default(), false, false);
        assert!(s1.is_empty());
    }

    #[test]
    fn greedy_merges_a_solid_section_into_six_quads() {
        let mut world = World::new();
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 {
                    world.set_block(x, y, z, BlockState::STONE);
                }
            }
        }
        let mut mesh = ChunkMesh::default();
        greedy_cube_mesh(&world, 0, 0, 0, &atlas(), BiomeColors::default(), false, &mut mesh);
        // A solid 16³ block shows only its six outer faces; greedy collapses each
        // into one quad → 6 quads = 24 verts / 36 indices (per-block would be
        // 6×256 quads). Interior faces are culled against opaque neighbours.
        assert_eq!(mesh.solid.indices.len(), 36, "six merged outer faces");
        assert_eq!(mesh.solid.vertices.len(), 24);
        // A merged face spans the full 16 blocks → a corner repeat-UV of 16, which
        // encodes to the Unorm16 max (16 / GREEDY_UV_SCALE = 1.0).
        assert!(
            mesh.solid.vertices.iter().any(|v| v.uv[0] == 65535 || v.uv[1] == 65535),
            "a merged-quad corner repeats across the full 16-block span"
        );
    }

    #[test]
    fn greedy_does_not_merge_unlike_blocks() {
        let count_quads = |split: bool| {
            let mut world = World::new();
            for x in 0..16 {
                for z in 0..16 {
                    let b = if split && x >= 8 { BlockState::GRASS } else { BlockState::STONE };
                    world.set_block(x, 0, z, b);
                }
            }
            let mut mesh = ChunkMesh::default();
            greedy_cube_mesh(&world, 0, 0, 0, &atlas(), BiomeColors::default(), false, &mut mesh);
            mesh.solid.vertices.len() / 4
        };
        // Splitting the floor into two materials forces the top/bottom faces into
        // separate quads, so the split mesh has strictly more quads than uniform.
        assert!(
            count_quads(true) > count_quads(false),
            "unlike blocks must not merge into one quad"
        );
    }

    #[test]
    fn sections_sum_to_the_whole_column() {
        // A block in each of two different sections: the per-section meshes must
        // together equal the whole-column mesh.
        let mut world = World::new();
        world.set_block(1, 2, 3, BlockState::STONE); // section 0
        world.set_block(4, 40, 5, BlockState::STONE); // section 2
        let column = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        let mut total = 0;
        for sy in 0..16 {
            total += build_section_mesh(&world, SectionPos::new(0, sy, 0), &atlas(), BiomeColors::default(), false, false)
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
        let s0 = build_section_mesh(&world, SectionPos::new(0, 0, 0), &atlas(), BiomeColors::default(), false, false);
        let s1 = build_section_mesh(&world, SectionPos::new(0, 1, 0), &atlas(), BiomeColors::default(), false, false);
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
        let sync = build_section_mesh(&world, SectionPos::new(0, 1, 0), &atlas(), BiomeColors::default(), false, false);
        let neighborhood = ChunkNeighborhood::snapshot(&world, ChunkPos::new(0, 0)).unwrap();
        let async_mesh = build_section_mesh_neighborhood(&neighborhood, 1, &atlas(), BiomeColors::default(), false, false);
        assert_eq!(sync.solid.vertices.len(), async_mesh.solid.vertices.len());
        assert_eq!(sync.solid.indices.len(), async_mesh.solid.indices.len());
        assert_eq!(async_mesh.solid.indices.len(), 5 * 6);
    }

    #[test]
    fn single_block_has_six_faces() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        assert_eq!(mesh.solid.vertices.len(), 24);
        assert_eq!(mesh.solid.indices.len(), 36);
    }

    #[test]
    fn smooth_lighting_is_flat_on_an_unoccluded_block() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        assert_eq!(mesh.solid.indices.len(), 60);
    }

    #[test]
    fn leaves_go_to_cutout_buffer_and_stay_opaque() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(18, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        assert_eq!(mesh.cutout.indices.len(), 2 * 36, "leaves keep their shared faces");
    }

    #[test]
    fn tall_grass_renders_as_cross() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(31, 1));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        assert!(mesh.is_empty());
        assert_eq!(BlockState::new(166, 0).render_shape(), RenderShape::None);
        assert!(!BlockState::new(166, 0).is_opaque_cube());
    }

    #[test]
    fn stairs_render_base_plus_quarter() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(53, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let lone = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        assert_eq!(lone.solid.vertices.len(), 24, "post only");

        world.set_block(0, 0, -1, BlockState::STONE);
        let joined = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        // Post + two north bars (3 boxes) for the fence, 6 faces for the stone.
        let fence_vertices = joined.solid.vertices.len() as i32 - 24;
        assert_eq!(fence_vertices, 3 * 24);
    }

    #[test]
    fn lone_pane_is_a_cross_and_connections_drop_to_arms() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(102, 0));
        let lone = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        // Post + 4 arm panels.
        assert_eq!(lone.cutout.vertices.len(), 5 * 24);

        world.set_block(-1, 0, 0, BlockState::STONE);
        let joined = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        // The flower carries a vanilla XZ position offset, so check the inset
        // against the offset model origin rather than the raw block corner.
        let (ox, _, oz) = cross_plant_offset(38, 0, 0);
        for v in &mesh.cutout.vertices {
            let p = vpos(v);
            assert!(p[0] - ox >= 0.05 - 0.02 && p[0] - ox <= 0.95 + 0.02);
            assert!(p[2] - oz >= 0.05 - 0.02 && p[2] - oz <= 0.95 + 0.02);
        }
    }

    #[test]
    fn cross_plant_uvs_stay_half_a_texel_inside_the_tile() {
        // With nearest filtering, UVs sitting exactly on the tile edge floor into
        // the neighbouring atlas tile; the green vertex tint then paints that bleed
        // as shimmering bright-green specks. Every cross UV must stay ≥ half a texel
        // inside its tile so no fetch escapes the sprite.
        let uv = atlas();
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(31, 1)); // tall grass
        let mesh = build_world_mesh(&world, &uv, BiomeColors::default(), false, false);
        let rect = uv.tile_rect(BlockState::new(31, 1).texture_name(BlockFace::Side));
        let (half_u, half_v) = (rect[2] / 32.0, rect[3] / 32.0);
        assert!(!mesh.cutout.vertices.is_empty());
        for v in &mesh.cutout.vertices {
            let u = v.uv[0] as f32 / 65535.0;
            let w = v.uv[1] as f32 / 65535.0;
            assert!(
                u >= rect[0] + half_u - 1e-4 && u <= rect[0] + rect[2] - half_u + 1e-4,
                "u {u} not inside tile {rect:?}"
            );
            assert!(
                w >= rect[1] + half_v - 1e-4 && w <= rect[1] + rect[3] - half_v + 1e-4,
                "v {w} not inside tile {rect:?}"
            );
        }
    }

    #[test]
    fn cube_face_uvs_stay_half_a_texel_inside_the_tile() {
        // emit_face maps full faces onto the tile edges; nearest sampling would
        // floor into the neighbour tile, bleeding along edges of glass/leaves/
        // panes/etc. Every face UV must stay ≥ half a texel inside its tile.
        let uv = atlas();
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(1, 0)); // stone: all six faces exposed
        let mesh = build_world_mesh(&world, &uv, BiomeColors::default(), false, false);
        let rect = uv.tile_rect(BlockState::new(1, 0).texture_name(BlockFace::Side));
        let (half_u, half_v) = (rect[2] / 32.0, rect[3] / 32.0);
        assert!(!mesh.solid.vertices.is_empty());
        for v in &mesh.solid.vertices {
            let u = v.uv[0] as f32 / 65535.0;
            let w = v.uv[1] as f32 / 65535.0;
            assert!(
                u >= rect[0] + half_u - 1e-4 && u <= rect[0] + rect[2] - half_u + 1e-4,
                "u {u} not inside tile {rect:?}"
            );
            assert!(
                w >= rect[1] + half_v - 1e-4 && w <= rect[1] + rect[3] - half_v + 1e-4,
                "v {w} not inside tile {rect:?}"
            );
        }
    }

    #[test]
    fn plant_offset_matches_vanilla_and_is_position_stable() {
        // Flowers/double plants offset in XZ only; tall grass/fern also sink in Y.
        // Values mirror BlockModelRenderer's y-independent hash at world (0,0,0),
        // where the hash is 0 → the minimum corner of each range.
        assert_eq!(cross_plant_offset(37, 0, 0), (-0.25, 0.0, -0.25));
        assert_eq!(cross_plant_offset(175, 0, 0), (-0.25, 0.0, -0.25));
        assert_eq!(cross_plant_offset(31, 0, 0), (-0.25, -0.2, -0.25));
        // Non-offset cross plants (sapling, dead bush, mushroom, sugar cane) stay put.
        for id in [6u16, 32, 39, 83] {
            assert_eq!(cross_plant_offset(id, 5, -3), (0.0, 0.0, 0.0));
        }
        // Deterministic per world position and within the documented ranges.
        for x in -40..40 {
            for z in -40..40 {
                let (ox, oy, oz) = cross_plant_offset(31, x, z);
                assert_eq!((ox, oy, oz), cross_plant_offset(31, x, z));
                assert!((-0.25..=0.25).contains(&ox) && (-0.25..=0.25).contains(&oz));
                assert!((-0.2..=0.0).contains(&oy));
            }
        }
    }

    #[test]
    fn slab_side_faces_crop_the_texture_vertically() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(44, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
        assert_eq!(mesh.solid.vertices.len(), 24, "piston body is a full cube");

        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(34, 1));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default(), false, false);
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
