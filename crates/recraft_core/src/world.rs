use std::collections::HashMap;

use crate::{BlockState, Chunk, ChunkPos, EntityId, EntityState};

#[derive(Debug, Default)]
pub struct World {
    chunks: HashMap<ChunkPos, Chunk>,
    entities: HashMap<EntityId, EntityState>,
}

impl World {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.chunks.values()
    }

    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos)
    }

    pub fn remove_chunk(&mut self, pos: ChunkPos) {
        self.chunks.remove(&pos);
    }

    pub fn chunk_mut_or_insert(&mut self, pos: ChunkPos) -> &mut Chunk {
        self.chunks.entry(pos).or_insert_with(|| Chunk::new(pos))
    }

    pub fn set_block(&mut self, x: i32, y: i32, z: i32, block: BlockState) {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        self.chunk_mut_or_insert(pos)
            .set_block(local_x, y, local_z, block);
    }

    pub fn block_at(&self, x: i32, y: i32, z: i32) -> BlockState {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let Some(chunk) = self.chunk(pos) else {
            return BlockState::AIR;
        };
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        chunk.get_block(local_x, y, local_z)
    }

    pub fn set_light(&mut self, x: i32, y: i32, z: i32, block_light: u8, sky_light: u8) {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        self.chunk_mut_or_insert(pos)
            .set_light(local_x, y, local_z, block_light, sky_light);
    }

    pub fn light_at(&self, x: i32, y: i32, z: i32) -> (u8, u8) {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let Some(chunk) = self.chunk(pos) else {
            return (0, 15);
        };
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        chunk.light_at(local_x, y, local_z)
    }

    pub fn upsert_entity(&mut self, entity: EntityState) {
        self.entities.insert(entity.id, entity);
    }

    pub fn entity(&self, id: EntityId) -> Option<&EntityState> {
        self.entities.get(&id)
    }

    pub fn entity_mut(&mut self, id: EntityId) -> Option<&mut EntityState> {
        self.entities.get_mut(&id)
    }
}

fn div_floor(value: i32, divisor: i32) -> i32 {
    let quotient = value / divisor;
    let remainder = value % divisor;
    if remainder != 0 && ((remainder < 0) != (divisor < 0)) {
        quotient - 1
    } else {
        quotient
    }
}

fn mod_floor(value: i32, divisor: i32) -> i32 {
    let remainder = value % divisor;
    if remainder < 0 {
        remainder + divisor.abs()
    } else {
        remainder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_coordinates_handle_negative_chunks() {
        let mut world = World::new();
        world.set_block(-1, 64, -1, BlockState::DIRT);
        assert_eq!(world.block_at(-1, 64, -1), BlockState::DIRT);
        assert_eq!(
            world
                .chunk(ChunkPos::new(-1, -1))
                .unwrap()
                .get_block(15, 64, 15),
            BlockState::DIRT
        );
    }

    #[test]
    fn world_light_handles_negative_chunks() {
        let mut world = World::new();
        world.set_light(-1, 64, -1, 3, 14);
        assert_eq!(world.light_at(-1, 64, -1), (3, 14));
        assert_eq!(
            world
                .chunk(ChunkPos::new(-1, -1))
                .unwrap()
                .light_at(15, 64, 15),
            (3, 14)
        );
    }
}
