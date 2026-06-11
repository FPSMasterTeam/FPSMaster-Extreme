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
        let color = [
            base_color[0] * face.light,
            base_color[1] * face.light,
            base_color[2] * face.light,
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

fn block_color(block: BlockState) -> [f32; 3] {
    match block.id {
        18 => [0.72, 1.0, 0.72],
        _ => [1.0, 1.0, 1.0],
    }
}

fn block_tile(block: BlockState, face: FaceTile) -> BlockTile {
    match block.id {
        1 => BlockTile::Stone,
        2 => match face {
            FaceTile::Top => BlockTile::GrassTop,
            FaceTile::Bottom => BlockTile::Dirt,
            FaceTile::Side => BlockTile::GrassSide,
        },
        3 => BlockTile::Dirt,
        12 => BlockTile::Sand,
        17 => BlockTile::OakLog,
        18 => BlockTile::OakLeaves,
        _ => BlockTile::Missing,
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
}
