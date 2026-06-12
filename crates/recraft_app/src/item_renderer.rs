//! First-person hand and held-item rendering, ported 1:1 from vanilla 1.8.9
//! (`ItemRenderer` + `RenderItem`, MCP-919). The exact GL transform chains
//! are replayed through glam matrices in GL view space (+X right, +Y up, −Z
//! forward) and the resulting vertices are mapped into world space along the
//! camera basis.
//!
//! Chains replicated verbatim:
//! - `doItemUsedTransformations` (swing translation) →
//!   `transformFirstPersonItem` (base pose + swing rotations) → the single
//!   2× scale → the model's firstperson display transform (identity for 1.8
//!   block models; `[0,-135,25]/[0,4,2]/1.7` for generated/handheld items) →
//!   `RenderItem.renderItem`'s `scale(0.5)` + `translate(-0.5,-0.5,-0.5)`.
//! - `renderPlayerArm` for the empty hand, including the `ModelRenderer`
//!   rotation point and the 0.1 rad idle z-roll.
//! - `rotateWithPlayerRotations`: the hand lags the view by 10% of the gap.

use glam::{Mat4, Vec3};
use recraft_core::{BlockFace, BlockState, RenderShape, Tint};
use recraft_render::texture::item_texture_name;
use recraft_render::{AtlasUv, BiomeColors, Camera, ModelMesh, Vertex};

use crate::game::FirstPersonView;

pub struct ItemRenderer;

impl ItemRenderer {
    /// Append the first-person arm to the model pass — only with an empty
    /// hand, exactly like vanilla `renderItemInFirstPerson`.
    pub fn render_arm(mesh: &mut ModelMesh, camera: &Camera, view: &FirstPersonView) {
        if view.item.is_some() {
            return;
        }
        let matrix = arm_matrix(view);
        mesh.push_arm_box(
            &|local| view_to_world(camera, matrix.transform_point3(local)),
            1.0,
        );
    }

    /// Build the held item's first-person geometry for this frame.
    pub fn build_held_item(
        camera: &Camera,
        view: &FirstPersonView,
        atlas: &AtlasUv,
    ) -> (Vec<Vertex>, Vec<u32>) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let Some(item) = view.item else {
            return (vertices, indices);
        };

        // Common head of the chain: swing translation, base first-person
        // pose, and the one 2x scale every first-person item receives.
        let mut m = first_person_base(view);
        do_item_used_transformations(&mut m, view.swing_progress);
        transform_first_person_item(&mut m, view.equip_progress, view.swing_progress);
        gl_scale(&mut m, 2.0, 2.0, 2.0);

        if (0..256).contains(&item.id) {
            let block = BlockState::new(item.id as u16, (item.damage.max(0) & 15) as u8);
            if block.is_air() {
                return (vertices, indices);
            }
            match block.render_shape() {
                // Flat blocks (torches, flowers, rails, ladders) use generated
                // sprite item models in vanilla.
                RenderShape::Cross | RenderShape::Rail | RenderShape::Ladder => {
                    apply_first_person_display(&mut m);
                    finish_item_model(&mut m);
                    let name = block.texture_name(BlockFace::Side).map(str::to_owned);
                    let tint = tint3(block.tint(BlockFace::Side));
                    push_sprite(&mut vertices, &mut indices, camera, &m, name.as_deref(), atlas, tint);
                }
                _ => {
                    // 1.8 block models have NO firstperson display entry —
                    // identity — so the pose comes purely from the code chain.
                    finish_item_model(&mut m);
                    push_block_cube(&mut vertices, &mut indices, camera, &m, block, atlas);
                }
            }
        } else if let Some(name) = item_texture_name(item.id) {
            apply_first_person_display(&mut m);
            finish_item_model(&mut m);
            let name = format!("items/{name}");
            push_sprite(
                &mut vertices,
                &mut indices,
                camera,
                &m,
                Some(&name),
                atlas,
                [1.0, 1.0, 1.0],
            );
        }
        (vertices, indices)
    }
}

// ─── GlStateManager-equivalent matrix ops (post-multiplied, like GL) ─────────

fn gl_translate(m: &mut Mat4, x: f32, y: f32, z: f32) {
    *m *= Mat4::from_translation(Vec3::new(x, y, z));
}

fn gl_rotate(m: &mut Mat4, degrees: f32, x: f32, y: f32, z: f32) {
    *m *= Mat4::from_axis_angle(Vec3::new(x, y, z), degrees.to_radians());
}

fn gl_scale(m: &mut Mat4, x: f32, y: f32, z: f32) {
    *m *= Mat4::from_scale(Vec3::new(x, y, z));
}

/// GL view space → world space along the camera basis (GL looks down −Z).
fn view_to_world(camera: &Camera, v: Vec3) -> Vec3 {
    let forward = camera.direction();
    let right = forward.cross(Vec3::Y).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    camera.position + right * v.x + up * v.y - forward * v.z
}

// ─── Vanilla transform chains ────────────────────────────────────────────────

/// `rotateWithPlayerRotations`: the whole first-person rig lags the camera.
fn first_person_base(view: &FirstPersonView) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    gl_rotate(&mut m, view.arm_lag_pitch, 1.0, 0.0, 0.0);
    gl_rotate(&mut m, view.arm_lag_yaw, 0.0, 1.0, 0.0);
    m
}

/// `doItemUsedTransformations(swingProgress)`: the swing translation.
fn do_item_used_transformations(m: &mut Mat4, swing: f32) {
    let pi = std::f32::consts::PI;
    let s = swing.clamp(0.0, 1.0);
    let f = -0.4 * (s.sqrt() * pi).sin();
    let f1 = 0.2 * (s.sqrt() * pi * 2.0).sin();
    let f2 = -0.2 * (s * pi).sin();
    gl_translate(m, f, f1, f2);
}

/// `transformFirstPersonItem(equipProgress, swingProgress)`.
fn transform_first_person_item(m: &mut Mat4, equip: f32, swing: f32) {
    let pi = std::f32::consts::PI;
    gl_translate(m, 0.56, -0.52, -0.719_999_97);
    gl_translate(m, 0.0, equip * -0.6, 0.0);
    gl_rotate(m, 45.0, 0.0, 1.0, 0.0);
    let s = swing.clamp(0.0, 1.0);
    let f = (s * s * pi).sin();
    let f1 = (s.sqrt() * pi).sin();
    gl_rotate(m, f * -20.0, 0.0, 1.0, 0.0);
    gl_rotate(m, f1 * -20.0, 0.0, 0.0, 1.0);
    gl_rotate(m, f1 * -80.0, 1.0, 0.0, 0.0);
    gl_scale(m, 0.4, 0.4, 0.4);
}

/// The 1.8 generated/handheld firstperson display transform
/// (rotation [0,-135,25], translation [0,4,2]/16, scale 1.7), applied in the
/// vanilla order: translate, rotate Y, rotate X, rotate Z, scale.
fn apply_first_person_display(m: &mut Mat4) {
    gl_translate(m, 0.0, 4.0 / 16.0, 2.0 / 16.0);
    gl_rotate(m, -135.0, 0.0, 1.0, 0.0);
    gl_rotate(m, 25.0, 0.0, 0.0, 1.0);
    gl_scale(m, 1.7, 1.7, 1.7);
}

/// `RenderItem.renderItem`: scale(0.5) then center the 0..1 model.
fn finish_item_model(m: &mut Mat4) {
    gl_scale(m, 0.5, 0.5, 0.5);
    gl_translate(m, -0.5, -0.5, -0.5);
}

/// `renderPlayerArm` (empty hand), including the `ModelRenderer` rotation
/// point translate and the 0.1 rad idle z-roll from `setRotationAngles`.
fn arm_matrix(view: &FirstPersonView) -> Mat4 {
    let pi = std::f32::consts::PI;
    let s = view.swing_progress.clamp(0.0, 1.0);
    let mut m = first_person_base(view);
    let f = -0.3 * (s.sqrt() * pi).sin();
    let f1 = 0.4 * (s.sqrt() * pi * 2.0).sin();
    let f2 = -0.4 * (s * pi).sin();
    gl_translate(&mut m, f, f1, f2);
    gl_translate(&mut m, 0.640_000_05, -0.6, -0.719_999_97);
    gl_translate(&mut m, 0.0, view.equip_progress * -0.6, 0.0);
    gl_rotate(&mut m, 45.0, 0.0, 1.0, 0.0);
    let f3 = (s * s * pi).sin();
    let f4 = (s.sqrt() * pi).sin();
    gl_rotate(&mut m, f4 * 70.0, 0.0, 1.0, 0.0);
    gl_rotate(&mut m, f3 * -20.0, 0.0, 0.0, 1.0);
    gl_translate(&mut m, -1.0, 3.6, 3.5);
    gl_rotate(&mut m, 120.0, 0.0, 0.0, 1.0);
    gl_rotate(&mut m, 200.0, 1.0, 0.0, 0.0);
    gl_rotate(&mut m, -135.0, 0.0, 1.0, 0.0);
    gl_translate(&mut m, 5.6, 0.0, 0.0);
    // ModelRenderer.render(0.0625): rotation point (-5, 2, 0) then the idle
    // wobble rotateAngleZ = 0.1 rad with all-zero animation inputs.
    gl_translate(&mut m, -5.0 * 0.0625, 2.0 * 0.0625, 0.0);
    gl_rotate(&mut m, 0.1_f32.to_degrees(), 0.0, 0.0, 1.0);
    m
}

// ─── Geometry ────────────────────────────────────────────────────────────────

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

/// Constant tints for tinted block faces (plains biome colors).
fn tint3(tint: Tint) -> [f32; 3] {
    let biome = BiomeColors::default();
    match tint {
        Tint::None => [1.0, 1.0, 1.0],
        Tint::Grass => biome.grass,
        Tint::Foliage => biome.foliage,
        Tint::Water => [0.247, 0.463, 0.894],
        Tint::Rgb(rgb) => rgb,
    }
}

/// A held block: the unit-cube model with its real per-face textures and the
/// vanilla baked diffuse (up 1.0, down 0.5, ±z 0.8, ±x 0.6), transformed by
/// the first-person chain.
fn push_block_cube(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    camera: &Camera,
    m: &Mat4,
    block: BlockState,
    atlas: &AtlasUv,
) {
    // Per face: 4 unit-cube corners in (bottom-left, top-left, top-right,
    // bottom-right) texture order, the texture face, and the diffuse shade.
    let v = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
    let faces: [([Vec3; 4], BlockFace, f32); 6] = [
        // up (+y): u→x, v→z
        (
            [v(0.0, 1.0, 1.0), v(0.0, 1.0, 0.0), v(1.0, 1.0, 0.0), v(1.0, 1.0, 1.0)],
            BlockFace::Top,
            1.0,
        ),
        // down (−y)
        (
            [v(0.0, 0.0, 0.0), v(0.0, 0.0, 1.0), v(1.0, 0.0, 1.0), v(1.0, 0.0, 0.0)],
            BlockFace::Bottom,
            0.5,
        ),
        // south (+z): u→x, v→1−y
        (
            [v(0.0, 0.0, 1.0), v(0.0, 1.0, 1.0), v(1.0, 1.0, 1.0), v(1.0, 0.0, 1.0)],
            BlockFace::Side,
            0.8,
        ),
        // north (−z): mirrored u
        (
            [v(1.0, 0.0, 0.0), v(1.0, 1.0, 0.0), v(0.0, 1.0, 0.0), v(0.0, 0.0, 0.0)],
            BlockFace::Side,
            0.8,
        ),
        // east (+x)
        (
            [v(1.0, 0.0, 1.0), v(1.0, 1.0, 1.0), v(1.0, 1.0, 0.0), v(1.0, 0.0, 0.0)],
            BlockFace::Side,
            0.6,
        ),
        // west (−x)
        (
            [v(0.0, 0.0, 0.0), v(0.0, 1.0, 0.0), v(0.0, 1.0, 1.0), v(0.0, 0.0, 1.0)],
            BlockFace::Side,
            0.6,
        ),
    ];
    for (corners, face, shade) in faces {
        let uvs = atlas.uv(block.texture_name(face));
        let tint = tint3(block.tint(face));
        let color = [tint[0] * shade, tint[1] * shade, tint[2] * shade, 1.0];
        let world = corners.map(|c| view_to_world(camera, m.transform_point3(c)));
        push_quad(vertices, indices, world, uvs, color);
    }
}

/// A held sprite item: vanilla `ItemModelGenerator` geometry — the 16×16
/// layer as front/back quads on a 1px slab from z 7.5/16 to 8.5/16 (the
/// per-pixel edge extrusion is omitted).
fn push_sprite(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    camera: &Camera,
    m: &Mat4,
    name: Option<&str>,
    atlas: &AtlasUv,
    tint: [f32; 3],
) {
    let rect = atlas.tile_rect(name);
    let uv = |u: f32, v: f32| [rect[0] + u * rect[2], rect[1] + v * rect[3]];
    let color = [tint[0], tint[1], tint[2], 1.0];

    // South face (z = 8.5/16), UV [0,0,16,16]: u→x, v→1−y.
    let z = 8.5 / 16.0;
    let corners = [
        Vec3::new(0.0, 0.0, z),
        Vec3::new(0.0, 1.0, z),
        Vec3::new(1.0, 1.0, z),
        Vec3::new(1.0, 0.0, z),
    ]
    .map(|c| view_to_world(camera, m.transform_point3(c)));
    push_quad(
        vertices,
        indices,
        corners,
        [uv(0.0, 1.0), uv(0.0, 0.0), uv(1.0, 0.0), uv(1.0, 1.0)],
        color,
    );

    // North face (z = 7.5/16), UV [16,0,0,16]: mirrored u.
    let z = 7.5 / 16.0;
    let corners = [
        Vec3::new(1.0, 0.0, z),
        Vec3::new(1.0, 1.0, z),
        Vec3::new(0.0, 1.0, z),
        Vec3::new(0.0, 0.0, z),
    ]
    .map(|c| view_to_world(camera, m.transform_point3(c)));
    push_quad(
        vertices,
        indices,
        corners,
        [uv(0.0, 1.0), uv(0.0, 0.0), uv(1.0, 0.0), uv(1.0, 1.0)],
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use recraft_protocol::v1_8_9::packets::SlotItem;
    use recraft_render::TextureAtlasImage;

    fn atlas() -> AtlasUv {
        TextureAtlasImage::load_default().uv_table()
    }

    fn camera() -> Camera {
        Camera::new(Vec3::new(0.0, 70.0, 0.0), 1.0)
    }

    fn view(id: Option<i16>) -> FirstPersonView {
        FirstPersonView {
            item: id.map(|id| SlotItem {
                id,
                count: 1,
                damage: 0,
            }),
            equip_progress: 0.0,
            swing_progress: 0.0,
            arm_lag_pitch: 0.0,
            arm_lag_yaw: 0.0,
        }
    }

    #[test]
    fn block_items_build_a_textured_cube() {
        let (vertices, indices) =
            ItemRenderer::build_held_item(&camera(), &view(Some(1)), &atlas());
        assert_eq!(vertices.len(), 24);
        assert_eq!(indices.len(), 36);
    }

    #[test]
    fn sprite_items_build_front_and_back_quads() {
        let uv = atlas();
        let (vertices, indices) =
            ItemRenderer::build_held_item(&camera(), &view(Some(276)), &uv);
        assert_eq!(vertices.len(), 8);
        assert_eq!(indices.len(), 12);
        assert!(!uv.is_missing_tile(Some("items/diamond_sword")));
    }

    #[test]
    fn flat_blocks_hold_as_sprites() {
        let (vertices, _) = ItemRenderer::build_held_item(&camera(), &view(Some(50)), &atlas());
        assert_eq!(vertices.len(), 8, "torch should render as a sprite");
    }

    #[test]
    fn empty_hand_builds_nothing() {
        let (vertices, indices) = ItemRenderer::build_held_item(&camera(), &view(None), &atlas());
        assert!(vertices.is_empty() && indices.is_empty());
    }

    #[test]
    fn rest_pose_matches_the_vanilla_anchor() {
        // With no swing/equip/lag, the block-model center (0.5,0.5,0.5) must
        // land exactly on transformFirstPersonItem's translation.
        let mut m = first_person_base(&view(None));
        do_item_used_transformations(&mut m, 0.0);
        transform_first_person_item(&mut m, 0.0, 0.0);
        gl_scale(&mut m, 2.0, 2.0, 2.0);
        finish_item_model(&mut m);
        let center = m.transform_point3(Vec3::splat(0.5));
        assert!((center.x - 0.56).abs() < 1.0e-6);
        assert!((center.y + 0.52).abs() < 1.0e-6);
        assert!((center.z + 0.72).abs() < 1.0e-5);
    }
}
