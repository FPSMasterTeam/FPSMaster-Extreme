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
        // The overlay pipeline back-face culls (CCW front). Across this
        // billboard's right/up basis the camera-facing side is clockwise, so it
        // must be wound (0,2,1)/(0,3,2) to present as the front face — otherwise
        // every particle (and xp orb) is culled and nothing draws.
        indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
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

    /// The particle tiles the simulation samples for the always-on effects must
    /// land on non-transparent texels of `particles.png`, or those particles
    /// draw as fully transparent quads (invisible). Guarded: when the asset
    /// isn't found (no resource pack in the test env) the check is skipped, so
    /// this only fails when a real sheet maps a sampled tile to empty space.
    #[test]
    fn sampled_particle_tiles_are_opaque() {
        let Some(img) =
            crate::texture::load_asset_image("assets/minecraft/textures/particle/particles.png")
        else {
            eprintln!("particles.png not found; skipping opacity check");
            return;
        };
        let (w, h) = (img.width(), img.height());
        // The sheet is a 16×16 tile grid (vanilla particles.png).
        let cols = 16u32;
        let tile = w / cols;
        let opaque_frac = |index: u32| {
            let (ox, oy) = (index % cols * tile, index / cols * tile);
            let mut opaque = 0u32;
            for ty in 0..tile {
                for tx in 0..tile {
                    let px = img.get_pixel(ox + tx, oy + ty);
                    if px.0[3] > 127 {
                        opaque += 1;
                    }
                }
            }
            opaque as f32 / (tile * tile) as f32
        };
        assert!(w == h && tile >= 1, "unexpected particles.png shape {w}x{h}");
        // CRIT (65) and FLAME (48): substantial opaque content.
        assert!(opaque_frac(65) > 0.2, "crit tile 65 nearly empty");
        assert!(opaque_frac(48) > 0.2, "flame tile 48 nearly empty");
        // SMOKE animates frames 7→0 over its life; frame 7 (its first/biggest
        // frame, tile 7) carries the bulk of the puff.
        assert!(opaque_frac(7) > 0.2, "smoke first frame (tile 7) nearly empty");
    }

    /// The billboard must present its CAMERA-FACING side as the front face, or
    /// the overlay pipeline's back-face culling drops every particle/xp orb.
    /// Reproduce the GPU winding test: project a particle directly ahead of the
    /// camera and assert the first triangle is CCW in NDC (positive signed
    /// area), matching the cube faces that survive the same cull.
    #[test]
    fn billboard_faces_camera_as_front_face() {
        let mut cam = camera();
        cam.yaw = -90.0;
        cam.pitch = 0.0;
        // Place the particle 3 blocks straight ahead of the camera.
        let p = ParticleBillboard {
            world_pos: (cam.position + cam.direction() * 3.0).into(),
            ..billboard()
        };
        let (vertices, indices) = build_particle_mesh(&cam, &[p]);
        let vp = cam.view_projection();
        let ndc = |i: u32| {
            let c = vp * Vec3::from(vertices[i as usize].position).extend(1.0);
            glam::Vec2::new(c.x / c.w, c.y / c.w)
        };
        let (a, b, c) = (ndc(indices[0]), ndc(indices[1]), ndc(indices[2]));
        let signed_area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        assert!(
            signed_area > 0.0,
            "front face (CCW in NDC) required, got signed area {signed_area}"
        );
    }
}
