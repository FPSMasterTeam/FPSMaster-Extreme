//! First-person rendering of the player's arm and held item (vanilla
//! `ItemRenderer`). Like vanilla, the bare arm only renders when the hand is
//! empty; with an item held only the item shows. Block items render as a
//! small upright cube with their real block textures (turned 45° from the
//! view like vanilla); other items render as their `textures/items/` sprite.
//! Item geometry is rebuilt each frame in block-atlas [`Vertex`] format and
//! drawn by the renderer's cutout pass.

use glam::Vec3;
use recraft_core::{BlockFace, BlockState, Tint};
use recraft_protocol::v1_8_9::packets::SlotItem;
use recraft_render::texture::item_texture_name;
use recraft_render::{AtlasUv, BiomeColors, Camera, ModelMesh, Vertex};

/// Skin-toned first-person arm color (alpha only; the skin texture tones it).
const ARM_COLOR: [f32; 4] = [0.86, 0.71, 0.58, 1.0];

pub struct ItemRenderer;

impl ItemRenderer {
    /// Append the first-person arm to the model pass — only with an empty
    /// hand, exactly like vanilla `renderItemInFirstPerson`.
    pub fn render_arm(mesh: &mut ModelMesh, camera: &Camera, swing: f32, held: Option<SlotItem>) {
        if held.is_none() {
            mesh.push_hand(camera, swing, ARM_COLOR);
        }
    }

    /// Build the held item's first-person geometry for this frame (empty when
    /// nothing renderable is held).
    pub fn build_held_item(
        camera: &Camera,
        swing: f32,
        held: Option<SlotItem>,
        atlas: &AtlasUv,
    ) -> (Vec<Vertex>, Vec<u32>) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let Some(item) = held else {
            return (vertices, indices);
        };
        if (0..256).contains(&item.id) {
            // Block item: the damage value carries the block meta (wool
            // colors, plank kinds, ...).
            let block = BlockState::new(item.id as u16, (item.damage.max(0) & 15) as u8);
            if !block.is_air() {
                build_block_in_hand(&mut vertices, &mut indices, camera, swing, block, atlas);
            }
        } else if let Some(name) = item_texture_name(item.id) {
            let name = format!("items/{name}");
            build_sprite_in_hand(&mut vertices, &mut indices, camera, swing, &name, atlas);
        }
        (vertices, indices)
    }
}

/// Camera basis plus the vanilla swing curves, shared by all hand poses.
struct HandFrame {
    eye: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    /// Main raise/jab arc: sin(sqrt(p)·π).
    arc: f32,
    /// Cross-screen pull, peaking later: sin(p²·π).
    pull: f32,
}

fn hand_frame(camera: &Camera, swing: f32) -> HandFrame {
    let pi = std::f32::consts::PI;
    let swing = swing.clamp(0.0, 1.0);
    let forward = camera.direction();
    let right = forward.cross(Vec3::Y).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    HandFrame {
        eye: camera.position,
        forward,
        right,
        up,
        arc: (swing.sqrt() * pi).sin(),
        pull: (swing * swing * pi).sin(),
    }
}

fn push_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    corners: [Vec3; 4],
    uvs: [[f32; 2]; 4],
    color: [f32; 4],
) {
    let base = vertices.len() as u32;
    for (corner, uv) in corners.iter().zip(uvs) {
        vertices.push(Vertex {
            position: (*corner).into(),
            color,
            uv,
        });
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Constant tints for the few tinted block faces (plains biome colors).
fn tint_color(tint: Tint) -> [f32; 3] {
    let biome = BiomeColors::default();
    match tint {
        Tint::None => [1.0, 1.0, 1.0],
        Tint::Grass => biome.grass,
        Tint::Foliage => biome.foliage,
        Tint::Water => [0.247, 0.463, 0.894],
        Tint::Rgb(rgb) => rgb,
    }
}

/// A held block: an upright cube turned 45° from the view yaw at the lower
/// right, with the block's real per-face textures and world-like face shades.
fn build_block_in_hand(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    camera: &Camera,
    swing: f32,
    block: BlockState,
    atlas: &AtlasUv,
) {
    let frame = hand_frame(camera, swing);

    // Horizontal view basis so the cube stays upright while the camera pitches.
    let mut flat_forward = Vec3::new(frame.forward.x, 0.0, frame.forward.z).normalize_or_zero();
    if flat_forward == Vec3::ZERO {
        flat_forward = Vec3::Z;
    }
    let flat_right = flat_forward.cross(Vec3::Y).normalize_or_zero();
    let axis_x = (flat_right + flat_forward).normalize_or_zero();
    let axis_z = (flat_forward - flat_right).normalize_or_zero();

    let center = frame.eye
        + frame.forward * (0.62 + 0.14 * frame.arc)
        + frame.right * (0.46 - 0.30 * frame.pull)
        + frame.up * (-0.50 + 0.26 * frame.arc);
    let h = 0.17;

    let (ax, az, ay) = (axis_x * h, axis_z * h, Vec3::Y * h);
    // Each face: corners in (bottom-left, top-left, top-right, bottom-right)
    // order to match the atlas UV corner convention, plus its texture face
    // and the world renderer's directional shade.
    let faces: [([Vec3; 4], BlockFace, f32); 6] = [
        // top
        (
            [center - ax + ay + az, center - ax + ay - az, center + ax + ay - az, center + ax + ay + az],
            BlockFace::Top,
            1.0,
        ),
        // bottom
        (
            [center - ax - ay - az, center - ax - ay + az, center + ax - ay + az, center + ax - ay - az],
            BlockFace::Bottom,
            0.55,
        ),
        // +x
        (
            [center + ax - ay + az, center + ax + ay + az, center + ax + ay - az, center + ax - ay - az],
            BlockFace::Side,
            0.75,
        ),
        // -x
        (
            [center - ax - ay - az, center - ax + ay - az, center - ax + ay + az, center - ax - ay + az],
            BlockFace::Side,
            0.75,
        ),
        // +z
        (
            [center + az - ay - ax, center + az + ay - ax, center + az + ay + ax, center + az - ay + ax],
            BlockFace::Side,
            0.88,
        ),
        // -z
        (
            [center - az - ay + ax, center - az + ay + ax, center - az + ay - ax, center - az - ay - ax],
            BlockFace::Side,
            0.88,
        ),
    ];
    for (corners, face, shade) in faces {
        let uvs = atlas.uv(block.texture_name(face));
        let tint = tint_color(block.tint(face));
        let color = [tint[0] * shade, tint[1] * shade, tint[2] * shade, 1.0];
        push_quad(vertices, indices, corners, uvs, color);
    }
}

/// A held sprite item (tools/food/...): the 16×16 sprite as a double-sided
/// quad angled up across the lower right of the view, following the same
/// swing curves as the arm.
fn build_sprite_in_hand(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    camera: &Camera,
    swing: f32,
    name: &str,
    atlas: &AtlasUv,
) {
    let frame = hand_frame(camera, swing);
    // Grip point (sprite bottom-left) at the hand; the sprite plane leans
    // toward the view center and tilts up-left, vanilla-style.
    let base = frame.eye
        + frame.forward * (0.58 + 0.12 * frame.arc)
        + frame.right * (0.40 - 0.30 * frame.pull)
        + frame.up * (-0.62 + 0.24 * frame.arc);
    let u_axis = (frame.right * 0.75 + frame.forward * 0.35 - frame.up * 0.05).normalize_or_zero()
        * 0.55;
    let v_axis = (frame.up * 0.80 - frame.right * 0.30 + frame.forward * 0.25).normalize_or_zero()
        * 0.55;
    let corners = [base, base + v_axis, base + u_axis + v_axis, base + u_axis];
    let uvs = atlas.uv(Some(name));
    push_quad(vertices, indices, corners, uvs, [1.0, 1.0, 1.0, 1.0]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use recraft_render::TextureAtlasImage;

    fn atlas() -> AtlasUv {
        TextureAtlasImage::load_default().uv_table()
    }

    fn camera() -> Camera {
        Camera::new(Vec3::new(0.0, 70.0, 0.0), 1.0)
    }

    fn item(id: i16) -> Option<SlotItem> {
        Some(SlotItem {
            id,
            count: 1,
            damage: 0,
        })
    }

    #[test]
    fn block_items_build_a_textured_cube() {
        let (vertices, indices) =
            ItemRenderer::build_held_item(&camera(), 0.0, item(1), &atlas());
        assert_eq!(vertices.len(), 24);
        assert_eq!(indices.len(), 36);
    }

    #[test]
    fn sprite_items_build_a_quad_with_a_real_tile() {
        let uv = atlas();
        let (vertices, indices) = ItemRenderer::build_held_item(&camera(), 0.0, item(276), &uv);
        assert_eq!(vertices.len(), 4);
        assert_eq!(indices.len(), 6);
        assert!(!uv.is_missing_tile(Some("items/diamond_sword")));
    }

    #[test]
    fn empty_hand_builds_nothing() {
        let (vertices, indices) = ItemRenderer::build_held_item(&camera(), 0.0, None, &atlas());
        assert!(vertices.is_empty() && indices.is_empty());
    }
}
