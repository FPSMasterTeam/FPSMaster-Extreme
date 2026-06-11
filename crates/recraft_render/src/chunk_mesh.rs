use bytemuck::{Pod, Zeroable};
use recraft_core::{BlockState, World};

use crate::texture::{tile_uv, BlockTile};

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
    pub uv: [f32; 2],
}

impl Vertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];

    pub fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ChunkMesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl ChunkMesh {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

#[derive(Debug, Clone, Copy)]
enum FaceTile {
    Side,
    Top,
    Bottom,
}

struct Face {
    normal: [i32; 3],
    corners: [[f32; 3]; 4],
    light: f32,
    tile_selector: FaceTile,
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
        tile_selector: FaceTile::Side,
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
        tile_selector: FaceTile::Side,
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
        tile_selector: FaceTile::Top,
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
        tile_selector: FaceTile::Bottom,
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
        tile_selector: FaceTile::Side,
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
        tile_selector: FaceTile::Side,
    },
];

pub fn build_world_mesh(world: &World) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();

    for chunk in world.chunks() {
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
                        let wx = base_x + x;
                        let wy = base_y + y;
                        let wz = base_z + z;
                        append_visible_faces(world, &mut mesh, wx, wy, wz, block);
                    }
                }
            }
        }
    }

    mesh
}

fn append_visible_faces(
    world: &World,
    mesh: &mut ChunkMesh,
    x: i32,
    y: i32,
    z: i32,
    block: BlockState,
) {
    let base_color = block_color(block);
    for face in FACES {
        let neighbor = world.block_at(x + face.normal[0], y + face.normal[1], z + face.normal[2]);
        if neighbor.is_opaque_cube() {
            continue;
        }

        let start = mesh.vertices.len() as u32;
        let light = face_light(world, x, y, z, face.normal);
        let color = [
            base_color[0] * face.light * light,
            base_color[1] * face.light * light,
            base_color[2] * face.light * light,
        ];
        let uvs = tile_uv(block_tile(block, face.tile_selector));
        for (corner, uv) in face.corners.into_iter().zip(uvs) {
            mesh.vertices.push(Vertex {
                position: [
                    x as f32 + corner[0],
                    y as f32 + corner[1],
                    z as f32 + corner[2],
                ],
                color,
                uv,
            });
        }
        mesh.indices
            .extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
    }
}

fn face_light(world: &World, x: i32, y: i32, z: i32, normal: [i32; 3]) -> f32 {
    let (block_light, sky_light) = world.light_at(x + normal[0], y + normal[1], z + normal[2]);
    let level = block_light.max(sky_light);
    // Vanilla uses a non-linear light table. This is a small first pass that
    // keeps dark areas readable while preserving the 0..15 chunk light signal.
    0.18 + (level as f32 / 15.0) * 0.82
}

fn block_color(block: BlockState) -> [f32; 3] {
    match block.id {
        18 => [0.72, 1.0, 0.72],
        _ => [1.0, 1.0, 1.0],
    }
}

fn block_tile(block: BlockState, face: FaceTile) -> BlockTile {
    match block.id {
        1 => stone_tile(block.meta),
        2 => match face {
            FaceTile::Top => BlockTile::GrassTop,
            FaceTile::Bottom => BlockTile::Dirt,
            FaceTile::Side => BlockTile::GrassSide,
        },
        3 => dirt_tile(block.meta, face),
        4 => BlockTile::Cobblestone,
        5 => planks_tile(block.meta),
        7 => BlockTile::Bedrock,
        12 => {
            if block.meta & 0x1 == 1 {
                BlockTile::RedSand
            } else {
                BlockTile::Sand
            }
        }
        13 => BlockTile::Gravel,
        14 => BlockTile::GoldOre,
        15 => BlockTile::IronOre,
        16 => BlockTile::CoalOre,
        17 => log_tile(block.meta & 0x3, face),
        18 => leaves_tile(block.meta & 0x3),
        21 => BlockTile::LapisOre,
        22 => BlockTile::LapisBlock,
        24 => sandstone_tile(block.meta, face),
        35 => wool_tile(block.meta),
        41 => BlockTile::GoldBlock,
        42 => BlockTile::IronBlock,
        45 => BlockTile::Brick,
        48 => BlockTile::MossyCobblestone,
        49 => BlockTile::Obsidian,
        56 => BlockTile::DiamondOre,
        57 => BlockTile::DiamondBlock,
        73 | 74 => BlockTile::RedstoneOre,
        79 => BlockTile::Ice,
        80 => BlockTile::Snow,
        82 => BlockTile::Clay,
        86 => pumpkin_tile(face),
        87 => BlockTile::Netherrack,
        88 => BlockTile::SoulSand,
        89 => BlockTile::Glowstone,
        98 => stone_brick_tile(block.meta),
        103 => melon_tile(face),
        110 => match face {
            FaceTile::Top => BlockTile::MyceliumTop,
            FaceTile::Bottom => BlockTile::Dirt,
            FaceTile::Side => BlockTile::MyceliumSide,
        },
        112 => BlockTile::NetherBrick,
        121 => BlockTile::EndStone,
        129 => BlockTile::EmeraldOre,
        133 => BlockTile::EmeraldBlock,
        152 => BlockTile::RedstoneBlock,
        155 => quartz_tile(block.meta, face),
        159 => stained_clay_tile(block.meta),
        161 => leaves_tile((block.meta & 0x1) + 4),
        162 => log_tile((block.meta & 0x1) + 4, face),
        172 => BlockTile::HardenedClay,
        173 => BlockTile::CoalBlock,
        174 => BlockTile::PackedIce,
        179 => red_sandstone_tile(block.meta, face),
        _ => BlockTile::Missing,
    }
}

fn stone_tile(meta: u8) -> BlockTile {
    match meta {
        1 => BlockTile::Granite,
        2 => BlockTile::PolishedGranite,
        3 => BlockTile::Diorite,
        4 => BlockTile::PolishedDiorite,
        5 => BlockTile::Andesite,
        6 => BlockTile::PolishedAndesite,
        _ => BlockTile::Stone,
    }
}

fn dirt_tile(meta: u8, face: FaceTile) -> BlockTile {
    match meta {
        1 => BlockTile::CoarseDirt,
        2 => match face {
            FaceTile::Top => BlockTile::PodzolTop,
            FaceTile::Bottom => BlockTile::Dirt,
            FaceTile::Side => BlockTile::PodzolSide,
        },
        _ => BlockTile::Dirt,
    }
}

fn planks_tile(meta: u8) -> BlockTile {
    match meta & 0x7 {
        1 => BlockTile::PlanksSpruce,
        2 => BlockTile::PlanksBirch,
        3 => BlockTile::PlanksJungle,
        4 => BlockTile::PlanksAcacia,
        5 => BlockTile::PlanksDarkOak,
        _ => BlockTile::PlanksOak,
    }
}

fn log_tile(kind: u8, face: FaceTile) -> BlockTile {
    let top = matches!(face, FaceTile::Top | FaceTile::Bottom);
    match (kind, top) {
        (1, false) => BlockTile::SpruceLogSide,
        (1, true) => BlockTile::SpruceLogTop,
        (2, false) => BlockTile::BirchLogSide,
        (2, true) => BlockTile::BirchLogTop,
        (3, false) => BlockTile::JungleLogSide,
        (3, true) => BlockTile::JungleLogTop,
        (4, false) => BlockTile::AcaciaLogSide,
        (4, true) => BlockTile::AcaciaLogTop,
        (5, false) => BlockTile::DarkOakLogSide,
        (5, true) => BlockTile::DarkOakLogTop,
        (_, false) => BlockTile::OakLogSide,
        (_, true) => BlockTile::OakLogTop,
    }
}

fn leaves_tile(kind: u8) -> BlockTile {
    match kind {
        1 => BlockTile::SpruceLeaves,
        2 => BlockTile::BirchLeaves,
        3 => BlockTile::JungleLeaves,
        4 => BlockTile::AcaciaLeaves,
        5 => BlockTile::DarkOakLeaves,
        _ => BlockTile::OakLeaves,
    }
}

fn sandstone_tile(meta: u8, face: FaceTile) -> BlockTile {
    match (meta, face) {
        (_, FaceTile::Top) => BlockTile::SandstoneTop,
        (_, FaceTile::Bottom) => BlockTile::SandstoneBottom,
        (1, FaceTile::Side) => BlockTile::SandstoneCarved,
        (2, FaceTile::Side) => BlockTile::SandstoneSmooth,
        _ => BlockTile::SandstoneSide,
    }
}

fn red_sandstone_tile(meta: u8, face: FaceTile) -> BlockTile {
    match (meta, face) {
        (_, FaceTile::Top) => BlockTile::RedSandstoneTop,
        (_, FaceTile::Bottom) => BlockTile::RedSandstoneBottom,
        (1, FaceTile::Side) => BlockTile::RedSandstoneCarved,
        (2, FaceTile::Side) => BlockTile::RedSandstoneSmooth,
        _ => BlockTile::RedSandstoneSide,
    }
}

fn wool_tile(meta: u8) -> BlockTile {
    match meta & 0xf {
        1 => BlockTile::WoolOrange,
        2 => BlockTile::WoolMagenta,
        3 => BlockTile::WoolLightBlue,
        4 => BlockTile::WoolYellow,
        5 => BlockTile::WoolLime,
        6 => BlockTile::WoolPink,
        7 => BlockTile::WoolGray,
        8 => BlockTile::WoolSilver,
        9 => BlockTile::WoolCyan,
        10 => BlockTile::WoolPurple,
        11 => BlockTile::WoolBlue,
        12 => BlockTile::WoolBrown,
        13 => BlockTile::WoolGreen,
        14 => BlockTile::WoolRed,
        15 => BlockTile::WoolBlack,
        _ => BlockTile::WoolWhite,
    }
}

fn pumpkin_tile(face: FaceTile) -> BlockTile {
    match face {
        FaceTile::Top | FaceTile::Bottom => BlockTile::PumpkinTop,
        FaceTile::Side => BlockTile::PumpkinSide,
    }
}

fn melon_tile(face: FaceTile) -> BlockTile {
    match face {
        FaceTile::Top | FaceTile::Bottom => BlockTile::MelonTop,
        FaceTile::Side => BlockTile::MelonSide,
    }
}

fn stone_brick_tile(meta: u8) -> BlockTile {
    match meta {
        1 => BlockTile::StoneBrickMossy,
        2 => BlockTile::StoneBrickCracked,
        3 => BlockTile::StoneBrickCarved,
        _ => BlockTile::StoneBrick,
    }
}

fn quartz_tile(meta: u8, face: FaceTile) -> BlockTile {
    match meta {
        1 => match face {
            FaceTile::Top | FaceTile::Bottom => BlockTile::QuartzChiseledTop,
            FaceTile::Side => BlockTile::QuartzChiseled,
        },
        2 => match face {
            FaceTile::Top | FaceTile::Bottom => BlockTile::QuartzPillarTop,
            FaceTile::Side => BlockTile::QuartzPillarSide,
        },
        _ => match face {
            FaceTile::Top => BlockTile::QuartzTop,
            FaceTile::Bottom => BlockTile::QuartzBottom,
            FaceTile::Side => BlockTile::QuartzSide,
        },
    }
}

fn stained_clay_tile(meta: u8) -> BlockTile {
    match meta & 0xf {
        1 => BlockTile::StainedClayOrange,
        2 => BlockTile::StainedClayMagenta,
        3 => BlockTile::StainedClayLightBlue,
        4 => BlockTile::StainedClayYellow,
        5 => BlockTile::StainedClayLime,
        6 => BlockTile::StainedClayPink,
        7 => BlockTile::StainedClayGray,
        8 => BlockTile::StainedClaySilver,
        9 => BlockTile::StainedClayCyan,
        10 => BlockTile::StainedClayPurple,
        11 => BlockTile::StainedClayBlue,
        12 => BlockTile::StainedClayBrown,
        13 => BlockTile::StainedClayGreen,
        14 => BlockTile::StainedClayRed,
        15 => BlockTile::StainedClayBlack,
        _ => BlockTile::StainedClayWhite,
    }
}

#[cfg(test)]
mod tests {
    use recraft_core::{BlockState, World};

    use super::*;

    #[test]
    fn single_block_has_six_faces() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world);
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
    }

    #[test]
    fn adjacent_blocks_cull_internal_faces() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        world.set_block(1, 0, 0, BlockState::STONE);
        let mesh = build_world_mesh(&world);
        assert_eq!(mesh.indices.len(), 60);
    }

    #[test]
    fn mesh_uses_world_light() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        world.set_light(0, 1, 0, 0, 0);
        let mesh = build_world_mesh(&world);
        let has_dark_lit_vertex = mesh
            .vertices
            .iter()
            .any(|vertex| (vertex.color[0] - 0.18).abs() < 0.001);
        assert!(has_dark_lit_vertex);
    }
}
