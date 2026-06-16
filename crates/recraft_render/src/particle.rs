//! Camera-facing particle billboards. The simulation lives in `recraft_app`
//! (it can see the world); this module only turns a flat list of
//! [`ParticleBillboard`]s into textured quads, mirroring the dropped-item path.
//! Every quad samples `assets/minecraft/textures/particle/particles.png` (a
//! 16×16 tile grid) and is rendered at full brightness with alpha blending.

use glam::Vec3;

use crate::{Camera, Vertex, FULLBRIGHT};

/// One particle to draw this frame: an interpolated world position, a
/// half-extent (world units), the four corner UVs into `particles.png`, and an
/// RGBA tint (alpha drives the blend).
#[derive(Debug, Clone, Copy)]
pub struct ParticleBillboard {
    pub world_pos: [f32; 3],
    pub size: f32,
    /// Corner UVs in `particles.png` space, in the vanilla `renderParticle`
    /// order: bottom-right, top-right, top-left, bottom-left.
    pub uv: [[f32; 2]; 4],
    pub color: [f32; 4],
}

/// Build camera-facing quads for every particle. The quad spans the camera's
/// right/up basis (like nametags and world items), so particles always face the
/// viewer regardless of look direction.
pub fn build_particle_mesh(
    camera: &Camera,
    particles: &[ParticleBillboard],
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    let forward = camera.direction();
    let right = forward.cross(Vec3::Y).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();

    for p in particles {
        let center = Vec3::from(p.world_pos);
        let h = p.size;
        // Corner order matches `ParticleBillboard::uv`: (-r,-u), (-r,+u),
        // (+r,+u), (+r,-u). The overlay pipeline doesn't cull these (a particle
        // can be seen from behind), so the winding only needs to be consistent.
        let corners = [
            center - right * h - up * h,
            center - right * h + up * h,
            center + right * h + up * h,
            center + right * h - up * h,
        ];
        let base = vertices.len() as u32;
        for (corner, uv) in corners.iter().zip(p.uv) {
            vertices.push(Vertex {
                position: (*corner).into(),
                color: p.color,
                uv,
                light: FULLBRIGHT,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    (vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera::new(Vec3::new(0.0, 70.0, 0.0), 1.0)
    }

    fn billboard() -> ParticleBillboard {
        ParticleBillboard {
            world_pos: [0.0, 70.0, -3.0],
            size: 0.1,
            uv: [[0.0, 0.0]; 4],
            color: [1.0, 1.0, 1.0, 1.0],
        }
    }

    #[test]
    fn builds_one_quad_per_particle() {
        let (vertices, indices) = build_particle_mesh(&camera(), &[billboard(), billboard()]);
        assert_eq!(vertices.len(), 8);
        assert_eq!(indices.len(), 12);
    }

    #[test]
    fn no_particles_builds_nothing() {
        let (vertices, indices) = build_particle_mesh(&camera(), &[]);
        assert!(vertices.is_empty() && indices.is_empty());
    }

    #[test]
    fn quad_centers_on_the_particle_position() {
        let (vertices, _) = build_particle_mesh(&camera(), &[billboard()]);
        let centroid: Vec3 =
            vertices.iter().map(|v| Vec3::from(v.position)).sum::<Vec3>() / vertices.len() as f32;
        assert!((centroid - Vec3::new(0.0, 70.0, -3.0)).length() < 1.0e-5);
    }
}
