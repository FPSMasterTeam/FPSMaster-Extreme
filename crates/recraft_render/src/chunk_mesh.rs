use bytemuck::{Pod, Zeroable};
use recraft_core::{
    collision, BlockFace, BlockState, Chunk, ChunkPos, RenderLayer, RenderShape, Tint, World,
};

use crate::texture::STAINED_COLORS;
use crate::AtlasUv;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    /// Tint multiplied with the sampled texel; alpha lets leaves/glass blend.
    pub color: [f32; 4],
    pub uv: [f32; 2],
}

impl Vertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x2];

    pub fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
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

/// A clone of one chunk plus its four horizontal neighbours — everything the
/// mesher needs (including border-face culling) to build a chunk mesh on a
/// worker thread without touching the live `World`.
pub struct ChunkNeighborhood {
    pos: ChunkPos,
    center: Chunk,
    /// -x, +x, -z, +z (None when the neighbour isn't loaded).
    neighbors: [Option<Chunk>; 4],
}

impl ChunkNeighborhood {
    /// Snapshot `pos` and its present neighbours out of `world`. Returns None if
    /// the centre chunk isn't loaded. The clones are cheap relative to meshing
    /// and let the actual mesh build happen off the render thread.
    pub fn snapshot(world: &World, pos: ChunkPos) -> Option<Self> {
        let center = world.chunk(pos)?.clone();
        let neighbors = [
            world.chunk(ChunkPos::new(pos.x - 1, pos.z)).cloned(),
            world.chunk(ChunkPos::new(pos.x + 1, pos.z)).cloned(),
            world.chunk(ChunkPos::new(pos.x, pos.z - 1)).cloned(),
            world.chunk(ChunkPos::new(pos.x, pos.z + 1)).cloned(),
        ];
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
        if cx == self.pos.x && cz == self.pos.z {
            return Some(&self.center);
        }
        if cz == self.pos.z {
            if cx == self.pos.x - 1 {
                return self.neighbors[0].as_ref();
            }
            if cx == self.pos.x + 1 {
                return self.neighbors[1].as_ref();
            }
        }
        if cx == self.pos.x {
            if cz == self.pos.z - 1 {
                return self.neighbors[2].as_ref();
            }
            if cz == self.pos.z + 1 {
                return self.neighbors[3].as_ref();
            }
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

impl MeshBuffers {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn push_quad(&mut self, corners: [[f32; 3]; 4], uvs: [[f32; 2]; 4], color: [f32; 4]) {
        let start = self.vertices.len() as u32;
        for (position, uv) in corners.into_iter().zip(uvs) {
            self.vertices.push(Vertex {
                position,
                color,
                uv,
            });
        }
        self.indices
            .extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
    }
}

/// A chunk's geometry split by render pass: opaque, alpha-tested cutout
/// (leaves/plants/glass — keeps the texture's transparent gaps), and
/// alpha-blended translucent (water/ice/stained glass).
#[derive(Debug, Default, Clone)]
pub struct ChunkMesh {
    pub solid: MeshBuffers,
    pub cutout: MeshBuffers,
    pub transparent: MeshBuffers,
}

impl ChunkMesh {
    pub fn is_empty(&self) -> bool {
        self.solid.is_empty() && self.cutout.is_empty() && self.transparent.is_empty()
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
        light: 0.78,
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
        light: 0.62,
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
        light: 0.48,
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
        light: 0.72,
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
        light: 0.72,
        face: BlockFace::Side,
    },
];

pub fn build_world_mesh(world: &World, atlas: &AtlasUv, biome: BiomeColors) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    for chunk in world.chunks() {
        append_chunk_mesh(world, chunk, &mut mesh, atlas, biome);
    }
    mesh
}

pub fn build_chunk_mesh(
    world: &World,
    pos: ChunkPos,
    atlas: &AtlasUv,
    biome: BiomeColors,
) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    if let Some(chunk) = world.chunk(pos) {
        append_chunk_mesh(world, chunk, &mut mesh, atlas, biome);
    }
    mesh
}

/// Build a chunk mesh from a self-contained neighbourhood snapshot. This is the
/// off-main-thread path: no live `World` is referenced, so it can run on a
/// worker thread.
pub fn build_chunk_mesh_neighborhood(
    neighborhood: &ChunkNeighborhood,
    atlas: &AtlasUv,
    biome: BiomeColors,
) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    append_chunk_mesh(neighborhood, &neighborhood.center, &mut mesh, atlas, biome);
    mesh
}

fn append_chunk_mesh<S: BlockSource>(
    source: &S,
    chunk: &Chunk,
    mesh: &mut ChunkMesh,
    atlas: &AtlasUv,
    biome: BiomeColors,
) {
    let base_x = chunk.position.x * 16;
    let base_z = chunk.position.z * 16;
    for section in chunk.sections() {
        let base_y = section.y() * 16;
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
                    };
                    append_block(mesh, &ctx, base_x + x, base_y + y, base_z + z, block);
                }
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
    }
}

fn buffer_for(mesh: &mut ChunkMesh, block: BlockState) -> &mut MeshBuffers {
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
        if neighbor.is_opaque_cube() || neighbor.id == block.id {
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
    buffer: &mut MeshBuffers,
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
    let light = face_light(ctx, nx, ny, nz);
    let tint = tint_color(block.tint(face.face), ctx.biome);
    let color = [
        tint[0] * face.light * light,
        tint[1] * face.light * light,
        tint[2] * face.light * light,
        alpha,
    ];
    warn_if_missing(ctx.atlas, block, face_context(face.face), texture);
    let rect = ctx.atlas.tile_rect(texture);
    let mut corners = [[0.0f32; 3]; 4];
    let mut uvs = [[0.0f32; 2]; 4];
    for (i, corner) in face.corners.iter().enumerate() {
        let px = lerp_axis(corner[0], mn[0], mx[0]);
        let py = lerp_axis(corner[1], mn[1], mx[1]);
        let pz = lerp_axis(corner[2], mn[2], mx[2]);
        corners[i] = [x as f32 + px, y as f32 + py, z as f32 + pz];
        let (u, v) = face_uv(face.normal, px, py, pz);
        uvs[i] = [rect[0] + u * rect[2], rect[1] + v * rect[3]];
    }
    buffer.push_quad(corners, uvs, color);
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

/// Cross-shaped plant: two diagonal double-sided quads (the transparent
/// pipeline disables back-face culling, so one quad per diagonal suffices).
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
    let color = [
        tint[0] * light,
        tint[1] * light,
        tint[2] * light,
        block.render_alpha(),
    ];
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
    buffer.push_quad(
        [
            [fx + lo, fy, fz + lo],
            [fx + lo, fy + 1.0, fz + lo],
            [fx + hi, fy + 1.0, fz + hi],
            [fx + hi, fy, fz + hi],
        ],
        uvs,
        color,
    );
    buffer.push_quad(
        [
            [fx + hi, fy, fz + lo],
            [fx + hi, fy + 1.0, fz + lo],
            [fx + lo, fy + 1.0, fz + hi],
            [fx + lo, fy, fz + hi],
        ],
        uvs,
        color,
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
    let color = [
        tint[0] * light,
        tint[1] * light,
        tint[2] * light,
        block.render_alpha(),
    ];

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
    buffer_for(mesh, block).push_quad(corners, uvs, color);
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
    let color = [
        tint[0] * light,
        tint[1] * light,
        tint[2] * light,
        block.render_alpha(),
    ];
    let texture = block.texture_name(BlockFace::Side);
    warn_if_missing(ctx.atlas, block, "its ladder texture", texture);
    let uvs = ctx.atlas.uv(texture);
    buffer_for(mesh, block).push_quad(corners, uvs, color);
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

fn face_light<S: BlockSource>(ctx: &BlockCtx<S>, x: i32, y: i32, z: i32) -> f32 {
    let (block_light, sky_light) = neighbor_light(ctx, x, y, z);
    let level = block_light.max(sky_light);
    // Keep dark areas readable while preserving the 0..15 chunk light signal.
    0.18 + (level as f32 / 15.0) * 0.82
}

#[cfg(test)]
mod tests {
    use recraft_core::{BlockState, World};

    use super::*;

    fn atlas() -> AtlasUv {
        // The mesher only needs name→index resolution; the default atlas covers
        // every registry texture.
        crate::TextureAtlasImage::load_default().uv_table()
    }

    #[test]
    fn single_block_has_six_faces() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        assert_eq!(mesh.solid.vertices.len(), 24);
        assert_eq!(mesh.solid.indices.len(), 36);
    }

    #[test]
    fn adjacent_blocks_cull_internal_faces() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        world.set_block(1, 0, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        assert_eq!(mesh.solid.indices.len(), 60);
    }

    #[test]
    fn leaves_go_to_cutout_buffer_and_stay_opaque() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(18, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        assert!(mesh.solid.is_empty());
        assert!(mesh.transparent.is_empty());
        assert_eq!(mesh.cutout.indices.len(), 36);
        // Cutout keeps full opacity (no global semi-transparency).
        assert!(mesh
            .cutout
            .vertices
            .iter()
            .all(|v| (v.color[3] - 1.0).abs() < 1.0e-6));
    }

    #[test]
    fn tall_grass_renders_as_cross() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(31, 1));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        // Two double-sided quads → 8 vertices, 12 indices, in the cutout pass.
        assert!(mesh.solid.is_empty());
        assert_eq!(mesh.cutout.vertices.len(), 8);
        assert_eq!(mesh.cutout.indices.len(), 12);
    }

    #[test]
    fn bottom_slab_is_half_height() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(44, 0));
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        let max_y = mesh
            .solid
            .vertices
            .iter()
            .map(|v| v.position[1])
            .fold(f32::MIN, f32::max);
        assert!((max_y - 0.5).abs() < 1.0e-6, "slab top was {max_y}");
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
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        assert!(mesh.is_empty());
        assert_eq!(BlockState::new(166, 0).render_shape(), RenderShape::None);
        assert!(!BlockState::new(166, 0).is_opaque_cube());
    }

    #[test]
    fn stairs_render_base_plus_quarter() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(53, 0)); // east-facing bottom stairs
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        // Two boxes (slab + quarter) x 6 faces x 4 vertices.
        assert_eq!(mesh.solid.vertices.len(), 48);
        let max_y = mesh
            .solid
            .vertices
            .iter()
            .map(|v| v.position[1])
            .fold(f32::MIN, f32::max);
        assert!((max_y - 1.0).abs() < 1.0e-6, "stair top was {max_y}");
        // The full-height quarter only spans x >= 0.5.
        assert!(mesh
            .solid
            .vertices
            .iter()
            .filter(|v| v.position[1] > 0.75)
            .all(|v| v.position[0] >= 0.5 - 1.0e-6));
    }

    #[test]
    fn lone_fence_is_a_post_and_neighbours_grow_arm_bars() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(85, 0));
        let lone = build_world_mesh(&world, &atlas(), BiomeColors::default());
        assert_eq!(lone.solid.vertices.len(), 24, "post only");

        world.set_block(0, 0, -1, BlockState::STONE);
        let joined = build_world_mesh(&world, &atlas(), BiomeColors::default());
        // Post + two north bars (3 boxes) for the fence, 6 faces for the stone.
        let fence_vertices = joined.solid.vertices.len() as i32 - 24;
        assert_eq!(fence_vertices, 3 * 24);
    }

    #[test]
    fn lone_pane_is_a_cross_and_connections_drop_to_arms() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(102, 0));
        let lone = build_world_mesh(&world, &atlas(), BiomeColors::default());
        // Post + 4 arm panels.
        assert_eq!(lone.cutout.vertices.len(), 5 * 24);

        world.set_block(-1, 0, 0, BlockState::STONE);
        let joined = build_world_mesh(&world, &atlas(), BiomeColors::default());
        // Stone cube (5 visible faces x 4 = 20... full 6 since pane isn't opaque)
        // plus pane post + single west arm.
        let pane_vertices = joined.cutout.vertices.len();
        assert_eq!(pane_vertices, 2 * 24);
    }

    #[test]
    fn rails_rotate_and_slope() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(66, 0)); // flat north-south
        world.set_block(1, 0, 0, BlockState::new(66, 2)); // ascending east
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        assert_eq!(mesh.cutout.vertices.len(), 8, "one quad per rail");
        let max_y = mesh
            .cutout
            .vertices
            .iter()
            .map(|v| v.position[1])
            .fold(f32::MIN, f32::max);
        assert!(
            (max_y - 1.0625).abs() < 1.0e-6,
            "ascending rail top was {max_y}"
        );
    }

    #[test]
    fn cross_plants_are_inset_from_the_corners() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(38, 0)); // poppy
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        for v in &mesh.cutout.vertices {
            assert!(v.position[0] >= 0.05 - 1.0e-6 && v.position[0] <= 0.95 + 1.0e-6);
            assert!(v.position[2] >= 0.05 - 1.0e-6 && v.position[2] <= 0.95 + 1.0e-6);
        }
    }

    #[test]
    fn slab_side_faces_crop_the_texture_vertically() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(44, 0)); // bottom slab
        let mesh = build_world_mesh(&world, &atlas(), BiomeColors::default());
        let uv = atlas();
        let rect = uv.tile_rect(BlockState::new(44, 0).texture_name(BlockFace::Side));
        // Side faces span y 0..0.5 → they must sample only v 0.5..1.0 of the
        // tile (the lower half). Identify side quads as 4-vertex groups whose
        // x or z coordinate is constant (vertical planes).
        let mut checked = 0;
        for quad in mesh.solid.vertices.chunks(4) {
            let constant = |axis: usize| {
                quad.iter()
                    .all(|v| v.position[axis] == quad[0].position[axis])
            };
            if constant(1) {
                continue; // top/bottom face
            }
            assert!(constant(0) || constant(2), "unexpected slanted quad");
            for v in quad {
                assert!(
                    v.uv[1] >= rect[1] + 0.5 * rect[3] - 1.0e-6,
                    "slab side sampled the upper texture half (v {})",
                    v.uv[1]
                );
            }
            checked += 1;
        }
        assert_eq!(checked, 4, "expected four side faces");
    }
}
