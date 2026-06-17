//! Client-side particle simulation (vanilla 1.8.9 `EntityFX`), driving the
//! camera-facing billboards the renderer draws. Particles are spawned from the
//! S2A SpawnParticle packet and from local block breaks, ticked at 20 Hz, and
//! turned into [`ParticleBillboard`]s (interpolated position + sprite UV + tint
//! + size) each frame.
//!
//! Two streams live here:
//! - The packet/`particles.png` stream ([`Particle`]), turned into
//!   [`ParticleBillboard`]s the renderer draws against the particle sheet.
//! - The block-break debris stream ([`BlockParticle`], vanilla
//!   `EntityDiggingFX`), which samples the BROKEN block's terrain tile. It can't
//!   reuse [`ParticleBillboard`] (that binds the particle sheet), so it is
//!   exposed as [`BlockDebris`] for the item renderer to billboard against the
//!   block atlas — the same atlas the dropped-item/falling-block passes use.
//!
//! Simplification vs. vanilla (acceptable for the MVP): no world collision —
//! every particle is treated as `no_clip`, so motion is never stopped by blocks
//! and ground friction is never applied. The motions themselves (gravity, drag,
//! rise) match vanilla.

use glam::Vec3;
use recraft_core::BlockState;
use recraft_render::ParticleBillboard;

/// The vanilla particle-sheet sub-tile span: a tile nominally spans 1/16 of the
/// sheet, but the UV is inset slightly (`0.0624375 < 0.0625`) to avoid bleeding
/// into the neighbouring tile (vanilla `EntityFX.renderParticle`).
const TILE_SPAN: f32 = 0.0624375;

/// A live particle. Positions are in render-space world units (f32); `pos` is
/// the current tick's position and `prev_pos` the previous tick's, lerped for
/// the partial-tick render. Physics constants mirror the matching `EntityFX`.
#[derive(Debug, Clone)]
struct Particle {
    pos: Vec3,
    prev_pos: Vec3,
    /// `motionX/Y/Z` in blocks per tick.
    vel: Vec3,
    color: [f32; 3],
    /// `particleScale`: the half-extent is `0.1 * scale` world units.
    scale: f32,
    age: i32,
    max_age: i32,
    /// `particleGravity`: `motionY -= 0.04 * gravity` each tick.
    gravity: f32,
    /// Air drag applied to all motion components each tick (vanilla 0.98, or
    /// 0.96 for the rising/animated types, 0.999 for lava, etc.).
    drag: f32,
    /// `motionY += rise` before drag (smoke/cloud rise, bubbles float up).
    rise: f32,
    /// Base sprite tile index into `particles.png` (column = i%16, row = i/16).
    tex_index: i32,
    /// When set, the tile walks `7 - age*8/max_age` along row 0 over the
    /// lifetime (vanilla smoke/cloud/spell). `tex_index` is then the row base.
    animates_frames: bool,
    /// When set, `scale` shrinks toward 0 over the lifetime (smoke/cloud/note/
    /// heart/crit reach full size first then... — see `render_scale`).
    grows_then: GrowKind,
}

/// How a particle's render scale evolves over its life (vanilla per-FX
/// `renderParticle` scale tweaks).
#[derive(Debug, Clone, Copy, PartialEq)]
enum GrowKind {
    /// Constant `scale` (flame uses a custom shrink handled below).
    None,
    /// `scale * clamp(age/max_age * 32, 0, 1)` — grows in over the first ~1/32
    /// of life then holds (smoke, cloud, crit, note, heart, reddust, spell).
    GrowIn,
    /// `scale * (1 - f*f*0.5)` where `f = age/max_age` (flame).
    Flame,
    /// `scale * (1 - f*f)` (lava).
    Lava,
}

/// A live block-break debris particle (vanilla `EntityDiggingFX`): a tiny quad
/// of the broken block's terrain texture flying outward and falling. Physics
/// mirror a generic gravity particle (gravity 0.04, drag 0.98, no_clip).
#[derive(Debug, Clone)]
struct BlockParticle {
    pos: Vec3,
    prev_pos: Vec3,
    vel: Vec3,
    age: i32,
    max_age: i32,
    /// The broken block whose top-face tile this debris samples.
    block: BlockState,
    /// Top-left corner of the 1/4-tile sub-region (in tile-fraction 0..0.75)
    /// this debris shows — vanilla `EntityDiggingFX` picks a random quarter of
    /// the block texture per particle.
    uv_off: [f32; 2],
}

/// One block-debris quad to draw this frame: its interpolated world position,
/// half-extent (world units), the broken block (its top-face tile is sampled),
/// and the 1/4-tile sub-region offset. The renderer billboards it against the
/// block atlas (see [`crate::item_renderer::ItemRenderer::build_block_particles`]).
#[derive(Debug, Clone, Copy)]
pub struct BlockDebris {
    pub world_pos: Vec3,
    pub size: f32,
    pub block: BlockState,
    pub uv_off: [f32; 2],
}

/// All live particles, ticked once per game tick.
pub struct ParticleSystem {
    particles: Vec<Particle>,
    block_particles: Vec<BlockParticle>,
    rng: Rng,
}

impl ParticleSystem {
    pub fn new() -> Self {
        Self {
            particles: Vec::new(),
            block_particles: Vec::new(),
            // A fixed seed keeps spawns deterministic across runs; particle
            // jitter is cosmetic so the exact stream doesn't matter.
            rng: Rng::new(0x9E37_79B9_7F4A_7C15),
        }
    }

    /// Advance every particle one tick (vanilla 20 Hz) and drop the dead ones.
    pub fn tick(&mut self) {
        for p in &mut self.particles {
            p.prev_pos = p.pos;
            p.age += 1;
            // motionY -= 0.04 * gravity; move (no_clip); drag. (No ground
            // friction: every particle is treated as no_clip — see module docs.)
            p.vel.y += p.rise;
            p.vel.y -= 0.04 * p.gravity;
            p.pos += p.vel;
            p.vel *= p.drag;
        }
        self.particles.retain(|p| p.age < p.max_age);
        // Block-break debris: vanilla EntityDiggingFX physics (gravity 0.04,
        // drag 0.98, no_clip).
        for p in &mut self.block_particles {
            p.prev_pos = p.pos;
            p.age += 1;
            p.vel.y -= 0.04;
            p.pos += p.vel;
            p.vel *= 0.98;
        }
        self.block_particles.retain(|p| p.age < p.max_age);
        // Bound the working set so a particle storm can't grow without limit.
        const MAX_PARTICLES: usize = 4000;
        if self.particles.len() > MAX_PARTICLES {
            let drop = self.particles.len() - MAX_PARTICLES;
            self.particles.drain(0..drop);
        }
        if self.block_particles.len() > MAX_PARTICLES {
            let drop = self.block_particles.len() - MAX_PARTICLES;
            self.block_particles.drain(0..drop);
        }
    }

    /// Spawn `count` particles of `type_id` (the S2A particle id) around `pos`
    /// with the packet's `offset` spread box and `speed`. `args` are the extra
    /// VarInts (block/item id for the *_CRACK / DUST types, currently unused).
    pub fn spawn(
        &mut self,
        type_id: i32,
        pos: Vec3,
        offset: Vec3,
        speed: f32,
        count: i32,
        _args: &[i32],
    ) {
        // Vanilla World.spawnParticle: each of `count` particles is jittered by
        // `offset * gaussian` and given velocity `speed * gaussian` per axis.
        // NOTE/REDSTONE/SPELL reuse the offset/speed fields as colour inputs,
        // handled inside `make_particle` (one particle, no positional jitter).
        let colour_carrying = matches!(type_id, 23 | 30 | 15 | 16 | 17);
        let n = if colour_carrying { 1 } else { count.max(1) };
        for _ in 0..n {
            let p_pos = if colour_carrying {
                pos
            } else {
                pos + Vec3::new(
                    offset.x * self.rng.gaussian(),
                    offset.y * self.rng.gaussian(),
                    offset.z * self.rng.gaussian(),
                )
            };
            let p_vel = if colour_carrying {
                Vec3::ZERO
            } else {
                Vec3::new(
                    speed * self.rng.gaussian(),
                    speed * self.rng.gaussian(),
                    speed * self.rng.gaussian(),
                )
            };
            let particle = self.make_particle(type_id, p_pos, p_vel, offset);
            self.particles.push(particle);
        }
    }

    /// Spawn a burst of block-texture debris when `block` is fully mined
    /// (vanilla `RenderGlobal.addBlockDestroyEffects`). Each particle samples a
    /// random 1/4-tile sub-region of the block's top-face texture and flies
    /// outward from the block centre while falling — a clearly-visible,
    /// client-side debris effect. No-op for air.
    pub fn spawn_block_debris(&mut self, block_x: i32, block_y: i32, block_z: i32, block: BlockState) {
        if block.is_air() {
            return;
        }
        let base = Vec3::new(block_x as f32, block_y as f32, block_z as f32);
        // Vanilla spawns a 4×4×4 grid (64) on a full break; ~24 reads as a dense
        // burst without flooding the working set.
        const COUNT: usize = 24;
        for _ in 0..COUNT {
            let off = Vec3::new(self.rng.next_f32(), self.rng.next_f32(), self.rng.next_f32());
            let pos = base + off;
            // motion = (offset_from_centre) * a small speed, like EntityDiggingFX
            // (which seeds motion from the particle's offset within the block).
            let vel = (off - Vec3::splat(0.5)) * 0.4;
            // A random quarter of the tile (offset 0, 0.25, 0.5, 0.75 per axis).
            let uv_off = [
                (self.rng.next_f32() * 4.0).floor() / 4.0,
                (self.rng.next_f32() * 4.0).floor() / 4.0,
            ];
            // EntityDiggingFX lifetime: (4 / (rand*0.9 + 0.1)).
            let max_age = (4.0 / (self.rng.next_f32() * 0.9 + 0.1)) as i32;
            self.block_particles.push(BlockParticle {
                pos,
                prev_pos: pos,
                vel,
                age: 0,
                max_age: max_age.max(1),
                block,
                uv_off,
            });
        }
    }

    /// This frame's block-break debris (interpolated by `tick_alpha`), for the
    /// item renderer to billboard against the block atlas.
    pub fn block_debris(&self, tick_alpha: f32) -> Vec<BlockDebris> {
        let a = tick_alpha.clamp(0.0, 1.0);
        self.block_particles
            .iter()
            .map(|p| BlockDebris {
                world_pos: p.prev_pos.lerp(p.pos, a),
                // Vanilla EntityDiggingFX half-extent ≈ 0.1 * particleScale; the
                // base particleScale is ~0.1..0.2, so ~0.05 world units here.
                size: 0.06,
                block: p.block,
                uv_off: p.uv_off,
            })
            .collect()
    }

    /// Build this frame's billboards, interpolating each particle between its
    /// previous and current tick position by `tick_alpha`.
    pub fn billboards(&self, tick_alpha: f32) -> Vec<ParticleBillboard> {
        let a = tick_alpha.clamp(0.0, 1.0);
        self.particles
            .iter()
            .map(|p| {
                let pos = p.prev_pos.lerp(p.pos, a);
                let tile = if p.animates_frames {
                    // Walk along row 0: 7 - age*8/max_age (clamped to 0..7).
                    let frame = (7 - p.age * 8 / p.max_age.max(1)).clamp(0, 7);
                    p.tex_index + frame
                } else {
                    p.tex_index
                };
                ParticleBillboard {
                    world_pos: pos.into(),
                    size: 0.1 * render_scale(p, a),
                    uv: tile_uv(tile),
                    color: [p.color[0], p.color[1], p.color[2], 1.0],
                }
            })
            .collect()
    }

    /// Construct one particle from the S2A id at a jittered position/velocity.
    /// Unknown ids fall back to a generic smoke so nothing panics.
    fn make_particle(&mut self, type_id: i32, pos: Vec3, vel: Vec3, offset: Vec3) -> Particle {
        // Vanilla EntityFX base ctor (shared defaults).
        let scale = (self.rng.next_f32() * 0.5 + 0.5) * 2.0;
        let max_age = (4.0 / (self.rng.next_f32() * 0.9 + 0.1)) as i32;
        let mut p = Particle {
            pos,
            prev_pos: pos,
            vel,
            color: [1.0, 1.0, 1.0],
            scale,
            age: 0,
            max_age: max_age.max(1),
            gravity: 0.0,
            drag: 0.98,
            rise: 0.0,
            tex_index: 0,
            animates_frames: false,
            grows_then: GrowKind::None,
        };

        match type_id {
            // FLAME(26): tex 48, no gravity, drag 0.96, longer life, shrinks.
            26 => {
                p.tex_index = 48;
                p.drag = 0.96;
                p.max_age = (8.0 / (self.rng.next_f32() * 0.8 + 0.2)) as i32 + 4;
                p.grows_then = GrowKind::Flame;
            }
            // SMOKE(11): dark grey, rises, drag 0.96, animated row 0.
            11 => {
                let g = self.rng.next_f32() * 0.3;
                p.color = [g, g, g];
                p.scale *= 0.75;
                p.rise = 0.004;
                p.drag = 0.96;
                p.max_age = (8.0 / (self.rng.next_f32() * 0.8 + 0.2)) as i32;
                p.animates_frames = true;
                p.grows_then = GrowKind::GrowIn;
            }
            // LARGE SMOKE(12): like smoke but 2.5× scale and life.
            12 => {
                let g = self.rng.next_f32() * 0.3;
                p.color = [g, g, g];
                p.scale *= 0.75 * 2.5;
                p.rise = 0.004;
                p.drag = 0.96;
                p.max_age = ((8.0 / (self.rng.next_f32() * 0.8 + 0.2)) * 2.5) as i32;
                p.animates_frames = true;
                p.grows_then = GrowKind::GrowIn;
            }
            // CRIT(9): tex 65, bright, drag 0.7, slight downward pull.
            9 => {
                let g = self.rng.next_f32() * 0.3 + 0.6;
                p.color = [g, g, g];
                p.scale *= 0.75;
                p.tex_index = 65;
                p.drag = 0.7;
                p.gravity = 0.5; // motionY -= 0.02 ≈ -0.04 * 0.5
                p.max_age = (6.0 / (self.rng.next_f32() * 0.8 + 0.6)) as i32;
                p.grows_then = GrowKind::GrowIn;
            }
            // MAGIC CRIT(10): tex 66, blue-green tint.
            10 => {
                let r = (self.rng.next_f32() * 0.3 + 0.6) * 0.3;
                let g = (self.rng.next_f32() * 0.3 + 0.6) * 0.8;
                let b = self.rng.next_f32() * 0.3 + 0.6;
                p.color = [r, g, b];
                p.scale *= 0.75;
                p.tex_index = 66;
                p.drag = 0.7;
                p.gravity = 0.5;
                p.max_age = (6.0 / (self.rng.next_f32() * 0.8 + 0.6)) as i32;
                p.grows_then = GrowKind::GrowIn;
            }
            // LAVA(27): tex 49, pops up then falls, near-zero drag, shrinks.
            27 => {
                p.tex_index = 49;
                p.vel.y = self.rng.next_f32() * 0.4 + 0.05;
                p.gravity = 0.75; // motionY -= 0.03 = -0.04 * 0.75
                p.drag = 0.999;
                p.scale *= self.rng.next_f32() * 2.0 + 0.2;
                p.max_age = (16.0 / (self.rng.next_f32() * 0.8 + 0.2)) as i32;
                p.grows_then = GrowKind::Lava;
            }
            // SPLASH(5) / WAKE(6) / DROPLET(39): tex 19 + rand(0..4), gravity.
            5 | 6 | 39 => {
                p.tex_index = 19 + (self.rng.next_f32() * 4.0) as i32;
                p.gravity = if type_id == 5 { 0.04 } else { 0.06 };
                p.scale *= 0.5;
                p.max_age = (8.0 / (self.rng.next_f32() * 0.8 + 0.2)) as i32;
            }
            // BUBBLE(4): tex 32, floats up, drag 0.85.
            4 => {
                p.tex_index = 32;
                p.rise = 0.002;
                p.drag = 0.85;
                p.scale *= self.rng.next_f32() * 0.6 + 0.2;
                p.max_age = (8.0 / (self.rng.next_f32() * 0.8 + 0.2)) as i32;
            }
            // PORTAL(24): tex rand(0..8), purple, no gravity, long life.
            24 => {
                p.tex_index = (self.rng.next_f32() * 8.0) as i32;
                let f = self.rng.next_f32();
                p.color = [f * 0.9, f * 0.3, f];
                p.max_age = (self.rng.next_f32() * 10.0) as i32 + 40;
            }
            // NOTE(23): tex 64, rises then drags hard; colour from offset.x.
            23 => {
                p.tex_index = 64;
                p.vel = Vec3::ZERO;
                p.vel.y = 0.2;
                p.drag = 0.66;
                let two_pi = std::f32::consts::PI * 2.0;
                let n = offset.x;
                p.color = [
                    (n * two_pi).sin() * 0.65 + 0.35,
                    ((n + 1.0 / 3.0) * two_pi).sin() * 0.65 + 0.35,
                    ((n + 2.0 / 3.0) * two_pi).sin() * 0.65 + 0.35,
                ];
                p.scale *= 0.75;
                p.max_age = 6;
                p.grows_then = GrowKind::GrowIn;
            }
            // HEART(34): tex 80, rises gently.
            34 => {
                p.tex_index = 80;
                p.vel = Vec3::ZERO;
                p.vel.y = 0.1;
                p.drag = 0.86;
                p.scale *= 0.75;
                p.max_age = 16;
                p.grows_then = GrowKind::GrowIn;
            }
            // ANGRY VILLAGER(20): tex 81. HAPPY VILLAGER(21): tex 82.
            20 => {
                p.tex_index = 81;
                p.scale *= 0.75;
                p.max_age = 8;
                p.grows_then = GrowKind::GrowIn;
            }
            21 => {
                p.tex_index = 82;
                p.scale *= 0.75;
                p.max_age = 8;
                p.grows_then = GrowKind::GrowIn;
            }
            // REDSTONE(30): animated row 0, colour from offset (green==0 -> red).
            30 => {
                p.animates_frames = true;
                let (mut r, g, b) = (offset.x, offset.y, offset.z);
                if g <= 0.0 {
                    r = 1.0;
                }
                let f = self.rng.next_f32() * 0.2 + 0.8;
                p.color = [
                    (self.rng.next_f32() * 0.2 + 0.8) * r * f,
                    (self.rng.next_f32() * 0.2 + 0.8) * g * f,
                    (self.rng.next_f32() * 0.2 + 0.8) * b * f,
                ];
                p.scale *= 0.75;
                p.max_age = (8.0 / (self.rng.next_f32() * 0.8 + 0.2)) as i32;
                p.drag = 0.96;
                p.grows_then = GrowKind::GrowIn;
            }
            // CLOUD(29): animated row 0, light grey, big, drag 0.96.
            29 => {
                let g = 1.0 - self.rng.next_f32() * 0.3;
                p.color = [g, g, g];
                p.scale *= 0.75 * 2.5;
                p.drag = 0.96;
                p.max_age = ((8.0 / (self.rng.next_f32() * 0.8 + 0.3)) * 2.5) as i32;
                p.animates_frames = true;
                p.grows_then = GrowKind::GrowIn;
            }
            // EXPLODE(0): animated row 0, bright, large random scale, rises.
            0 => {
                let g = self.rng.next_f32() * 0.3 + 0.7;
                p.color = [g, g, g];
                p.scale = self.rng.next_f32() * self.rng.next_f32() * 6.0 + 1.0;
                p.rise = 0.004;
                p.drag = 0.9;
                p.max_age = (16.0 / (self.rng.next_f32() * 0.8 + 0.2)) as i32 + 2;
                p.animates_frames = true;
            }
            // SPELL(13-17): animated from base 128; mob-spell colours from offset.
            13..=17 => {
                p.tex_index = 128;
                p.animates_frames = true;
                p.vel.y *= 0.2;
                if matches!(type_id, 15..=17) {
                    p.color = [offset.x, offset.y, offset.z];
                }
                p.scale *= 0.75;
                p.rise = 0.004;
                p.drag = 0.96;
                p.max_age = (8.0 / (self.rng.next_f32() * 0.8 + 0.2)) as i32;
                p.grows_then = GrowKind::GrowIn;
            }
            // ITEM_CRACK(36) / BLOCK_CRACK(37) / BLOCK_DUST(38): generic grey
            // puff (block-texture sprites are out of scope for the MVP — see
            // module docs). Small, gravity-affected debris.
            36..=38 => {
                let g = 0.6;
                p.color = [g, g, g];
                p.scale *= 0.5;
                p.gravity = 1.0;
                p.tex_index = 0;
            }
            // Unimplemented ids fall back to a generic smoke-like puff.
            _ => {
                let g = self.rng.next_f32() * 0.3;
                p.color = [g, g, g];
                p.scale *= 0.75;
                p.rise = 0.004;
                p.drag = 0.96;
                p.max_age = (8.0 / (self.rng.next_f32() * 0.8 + 0.2)) as i32;
                p.animates_frames = true;
                p.grows_then = GrowKind::GrowIn;
            }
        }
        p.max_age = p.max_age.max(1);
        p
    }
}

impl Default for ParticleSystem {
    fn default() -> Self {
        Self::new()
    }
}

/// The render-time scale for a particle at partial-tick `a` (vanilla per-FX
/// `renderParticle` evolves the scale over the lifetime).
fn render_scale(p: &Particle, a: f32) -> f32 {
    let f = (p.age as f32 + a) / p.max_age as f32;
    match p.grows_then {
        GrowKind::None => p.scale,
        GrowKind::GrowIn => p.scale * (f * 32.0).clamp(0.0, 1.0),
        GrowKind::Flame => p.scale * (1.0 - f * f * 0.5),
        GrowKind::Lava => p.scale * (1.0 - f * f),
    }
}

/// UV for the four corners of tile `index` in the 16×16 `particles.png` grid,
/// in the vanilla `renderParticle` corner order (bottom-right, top-right,
/// top-left, bottom-left).
fn tile_uv(index: i32) -> [[f32; 2]; 4] {
    let col = (index % 16) as f32;
    let row = (index / 16) as f32;
    let u0 = col / 16.0;
    let u1 = u0 + TILE_SPAN;
    let v0 = row / 16.0;
    let v1 = v0 + TILE_SPAN;
    [[u1, v1], [u1, v0], [u0, v0], [u0, v1]]
}

/// A tiny xorshift64* PRNG. Particle randomness is purely cosmetic (spawn
/// jitter, scale, lifetime), so this needs no statistical quality — only to be
/// dependency-free and fast.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed | 1, // never zero (xorshift fixed point)
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform `f32` in `[0, 1)`, like Java's `Random.nextFloat`.
    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }

    /// A rough standard-normal sample (sum of three uniforms, centred), standing
    /// in for vanilla `Random.nextGaussian` used by `World.spawnParticle`.
    fn gaussian(&mut self) -> f32 {
        (self.next_f32() + self.next_f32() + self.next_f32() - 1.5) * 1.1547
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flame's render scale shrinks as it ages (vanilla `1 - f*f*0.5`).
    #[test]
    fn flame_loses_scale_over_life() {
        let mut sys = ParticleSystem::new();
        sys.spawn(26, Vec3::ZERO, Vec3::ZERO, 0.0, 1, &[]);
        let young = render_scale(&sys.particles[0], 0.0);
        // Age it to the end of its life.
        for _ in 0..sys.particles[0].max_age - 1 {
            sys.tick();
        }
        let old = render_scale(&sys.particles[0], 0.0);
        assert!(old < young, "flame {old} should be smaller than {young}");
    }

    /// A particle is removed once it reaches its max age.
    #[test]
    fn particle_dies_at_max_age() {
        let mut sys = ParticleSystem::new();
        sys.spawn(26, Vec3::ZERO, Vec3::ZERO, 0.0, 1, &[]);
        let max_age = sys.particles[0].max_age;
        assert_eq!(sys.particles.len(), 1);
        for _ in 0..max_age {
            sys.tick();
        }
        assert!(sys.particles.is_empty(), "particle should be dead by max_age");
    }

    /// Gravity > 0 accelerates a particle downward (its Y velocity decreases).
    #[test]
    fn gravity_accelerates_downward() {
        let mut sys = ParticleSystem::new();
        // LAVA has gravity > 0; start from rest by zeroing the initial pop.
        sys.spawn(27, Vec3::ZERO, Vec3::ZERO, 0.0, 1, &[]);
        sys.particles[0].vel = Vec3::ZERO;
        let y_before = sys.particles[0].pos.y;
        sys.tick();
        sys.tick();
        assert!(sys.particles[0].vel.y < 0.0, "Y velocity should be negative");
        assert!(sys.particles[0].pos.y < y_before, "should have fallen");
    }

    /// No-gravity particles don't accelerate downward (flame floats).
    #[test]
    fn no_gravity_does_not_fall() {
        let mut sys = ParticleSystem::new();
        sys.spawn(26, Vec3::ZERO, Vec3::ZERO, 0.0, 1, &[]);
        sys.particles[0].vel = Vec3::ZERO;
        sys.tick();
        assert_eq!(sys.particles[0].vel.y, 0.0);
    }

    #[test]
    fn block_debris_spawns_a_burst_with_quarter_tile_uvs() {
        let mut sys = ParticleSystem::new();
        sys.spawn_block_debris(10, 64, -3, BlockState::STONE);
        // A dense burst, kept on its own stream (not the particles.png list).
        assert_eq!(sys.block_particles.len(), 24);
        assert!(sys.particles.is_empty());
        let debris = sys.block_debris(0.0);
        assert_eq!(debris.len(), 24);
        for d in &debris {
            // Each samples one quarter of the tile: offset ∈ {0, .25, .5, .75}
            // on each axis, so the +0.25 sub-rect always stays inside the tile.
            for off in d.uv_off {
                assert!((0.0..=0.75).contains(&off), "uv_off {off} outside tile");
                assert_eq!(off * 4.0, (off * 4.0).round(), "uv_off {off} not a quarter step");
            }
            assert_eq!(d.block, BlockState::STONE);
        }
    }

    #[test]
    fn block_debris_air_is_a_noop() {
        let mut sys = ParticleSystem::new();
        sys.spawn_block_debris(0, 0, 0, BlockState::AIR);
        assert!(sys.block_particles.is_empty());
    }

    #[test]
    fn block_debris_falls_and_expires() {
        let mut sys = ParticleSystem::new();
        sys.spawn_block_debris(0, 64, 0, BlockState::STONE);
        let y0 = sys.block_particles[0].pos.y;
        for _ in 0..3 {
            sys.tick();
        }
        // Gravity pulls debris down over a few ticks.
        assert!(sys.block_particles[0].pos.y < y0 + 0.1);
        // Tick well past the max lifetime — all debris must be gone.
        for _ in 0..60 {
            sys.tick();
        }
        assert!(sys.block_particles.is_empty());
    }

    #[test]
    fn unknown_id_falls_back_without_panicking() {
        let mut sys = ParticleSystem::new();
        sys.spawn(999, Vec3::ZERO, Vec3::ZERO, 0.0, 3, &[]);
        assert_eq!(sys.particles.len(), 3);
    }
}
