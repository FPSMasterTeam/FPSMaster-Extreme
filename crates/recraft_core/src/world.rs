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

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos)
    }

    /// Whether the chunk containing the given world-space block column is
    /// loaded. Used to avoid running local physics (and falling) through
    /// terrain the server has not sent yet.
    pub fn is_block_column_loaded(&self, x: i32, z: i32) -> bool {
        self.chunks
            .contains_key(&ChunkPos::new(div_floor(x, 16), div_floor(z, 16)))
    }

    pub fn remove_chunk(&mut self, pos: ChunkPos) {
        self.chunks.remove(&pos);
    }

    pub fn chunk_mut_or_insert(&mut self, pos: ChunkPos) -> &mut Chunk {
        self.chunks.entry(pos).or_insert_with(|| Chunk::new(pos))
    }

    /// Load a fully-decoded chunk section in one shot. `section_y` is 0..16.
    /// `blocks` is 4096 raw 1.8 states (y*256 + z*16 + x order). `block_light`
    /// and `sky_light` are 2048 packed nibbles each. Creates the chunk/section
    /// if absent. No per-block hashmap work.
    pub fn load_section(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        section_y: i32,
        blocks: &[u16],
        block_light: &[u8],
        sky_light: &[u8],
    ) {
        let chunk = self.chunk_mut_or_insert(ChunkPos::new(chunk_x, chunk_z));
        let section = chunk.section_mut_or_insert(section_y);
        section.fill_blocks_raw(blocks);
        section.fill_light_nibbles(block_light, sky_light);
    }

    pub fn set_block(&mut self, x: i32, y: i32, z: i32, block: BlockState) {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        self.chunk_mut_or_insert(pos)
            .set_block(local_x, y, local_z, block);
    }

    pub fn set_block_if_chunk_loaded(&mut self, x: i32, y: i32, z: i32, block: BlockState) -> bool {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return false;
        };
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        chunk.set_block(local_x, y, local_z, block);
        true
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

    pub fn remove_entity(&mut self, id: EntityId) {
        self.entities.remove(&id);
    }

    pub fn entity(&self, id: EntityId) -> Option<&EntityState> {
        self.entities.get(&id)
    }

    pub fn entity_mut(&mut self, id: EntityId) -> Option<&mut EntityState> {
        self.entities.get_mut(&id)
    }

    pub fn entities_mut(&mut self) -> impl Iterator<Item = &mut EntityState> {
        self.entities.values_mut()
    }

    pub fn entities(&self) -> impl Iterator<Item = &EntityState> {
        self.entities.values()
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
    fn load_section_bulk_loads_blocks_and_light() {
        let mut world = World::new();
        let mut blocks = vec![0u16; crate::chunk::SECTION_VOLUME];
        // STONE at local (x=5, y=6, z=7) within section 4 of chunk (-1, -1).
        blocks[6 * 256 + 7 * 16 + 5] = 1 << 4;
        let mut block_light = vec![0u8; 2048];
        let mut sky_light = vec![0u8; 2048];
        let light_index = 6 * 256 + 7 * 16 + 5; // 1653, odd -> high nibble
        block_light[light_index / 2] = 0x90; // high nibble = 9
        sky_light[light_index / 2] = 0xd0; // high nibble = 13
        world.load_section(-1, -1, 4, &blocks, &block_light, &sky_light);

        let (world_x, world_y, world_z) = (-16 + 5, 4 * 16 + 6, -16 + 7);
        assert_eq!(world.block_at(world_x, world_y, world_z), BlockState::STONE);
        assert_eq!(
            world.block_at(world_x, world_y + 1, world_z),
            BlockState::AIR
        );
        assert_eq!(world.light_at(world_x, world_y, world_z), (9, 13));

        // Also exercise a non-negative chunk.
        world.load_section(2, 3, 0, &blocks, &block_light, &sky_light);
        assert_eq!(world.block_at(2 * 16 + 5, 6, 3 * 16 + 7), BlockState::STONE);
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
