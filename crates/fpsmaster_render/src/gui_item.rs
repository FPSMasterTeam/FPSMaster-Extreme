//! 3D block icons for inventory/hotbar slots (vanilla `RenderItem`'s GUI block
//! transform). A block's `render_boxes()` are emitted as textured cuboids and
//! baked straight into clip space through the isometric GUI pose, so the GPU
//! cube pass can draw them with a pass-through (identity) camera. Non-box shapes
//! (cross plants, rails, ladders) have no boxes and fall back to the flat icon.

use glam::{Mat3, Vec3};
use fpsmaster_core::{BlockFace, BlockState, Tint};

use crate::ui::UiRect;
use crate::{AtlasUv, BiomeColors, Vertex, FULLBRIGHT};

/// One visible face of the unit cube: outward normal, the four `0/1` corners,
/// the texture face it samples and the vanilla baked diffuse shade.
struct GuiFace {
    normal: [i32; 3],
    corners: [[f32; 3]; 4],
    face: BlockFace,
    shade: f32,
}

const FACES: [GuiFace; 6] = [
    GuiFace {
        normal: [1, 0, 0],
        corners: [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [1.0, 0.0, 1.0]],
        face: BlockFace::Side,
        shade: 0.78,
    },
    GuiFace {
        normal: [-1, 0, 0],
        corners: [[0.0, 0.0, 1.0], [0.0, 1.0, 1.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]],
        face: BlockFace::Side,
        shade: 0.62,
    },
    GuiFace {
        normal: [0, 1, 0],
        corners: [[0.0, 1.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
        face: BlockFace::Top,
        shade: 1.0,
    },
    GuiFace {
        normal: [0, -1, 0],
        corners: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]],
        face: BlockFace::Bottom,
        shade: 0.5,
    },
    GuiFace {
        normal: [0, 0, 1],
        corners: [[1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0], [0.0, 0.0, 1.0]],
        face: BlockFace::Side,
        shade: 0.72,
    },
    GuiFace {
        normal: [0, 0, -1],
        corners: [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
        face: BlockFace::Side,
        shade: 0.72,
    },
];

/// Whether `id`/`damage` is a block that should render as a 3D icon (it has box
/// geometry). Cross/rail/ladder/air blocks have no boxes → keep the flat icon.
pub fn is_block_icon(id: i16, damage: i16) -> Option<(u16, u8)> {
    if !(1..256).contains(&id) {
        return None;
    }
    let meta = (damage.max(0) & 15) as u8;
    let block = BlockState::new(id as u16, meta);
    if block.is_air() || block.render_boxes().as_slice().is_empty() {
        return None;
    }
    Some((id as u16, meta))
}

/// The 3D block GUI pose, matching vanilla 1.8's `RenderItem.setupGuiTransform`
/// for `isGui3d` items: `rotate(210°, X)` then `rotate(-135°, Y)` (applied as
/// Rx(210)·Ry(-135)), plus the `scale(1, 1, -1)` Z-flip. (1.8 cubes have no model
/// `display.gui` transform — the `[30, 225, 0]` pose only exists from 1.9+, so the
/// previous value showed the block from the opposite corner.) The `diag(1,-1,1)`
/// factor folds vanilla's Y-down GUI projection / Z-flip into our orthographic
/// `to_clip` mapping (which treats +y as up), so the result lands pixel-identical.
fn gui_rotation() -> Mat3 {
    Mat3::from_diagonal(Vec3::new(1.0, -1.0, 1.0))
        * Mat3::from_rotation_x(210.0_f32.to_radians())
        * Mat3::from_rotation_y(-135.0_f32.to_radians())
}

/// Append a block's 3D icon geometry (clip-space `Vertex`es) for slot `rect`.
pub fn append_block_icon(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    block: BlockState,
    rect: UiRect,
    surface: (f32, f32),
    atlas: &AtlasUv,
    biome: &BiomeColors,
) {
    let rot = gui_rotation();
    // The unit cube edge in physical px (vanilla scales the 16px model by 0.625).
    let edge = rect.width as f32 * 0.625;
    let center = (
        rect.x as f32 + rect.width as f32 * 0.5,
        rect.y as f32 + rect.height as f32 * 0.5,
    );
    // Block-local [0,1] point → wgpu clip space (identity camera in the pass).
    let to_clip = |p: Vec3| {
        let r = rot * (p - Vec3::splat(0.5));
        let sx = center.0 + r.x * edge;
        let sy = center.1 - r.y * edge;
        [
            sx / surface.0 * 2.0 - 1.0,
            1.0 - sy / surface.1 * 2.0,
            // Vanilla depth: faces nearer the viewer have a more negative rotated
            // z, so map smaller z → smaller (nearer) clip depth.
            (0.5 + r.z * 0.5).clamp(0.0, 1.0),
        ]
    };

    for shape_box in block.render_boxes().as_slice() {
        let mn = shape_box.min.map(|v| v as f32);
        let mx = shape_box.max.map(|v| v as f32);
        for face in &FACES {
            let rect_uv = atlas.tile_rect(block.texture_name(face.face));
            let tint = tint3(block.tint(face.face), biome);
            let color = [tint[0] * face.shade, tint[1] * face.shade, tint[2] * face.shade, 1.0];
            let base = vertices.len() as u32;
            for corner in face.corners {
                let px = if corner[0] < 0.5 { mn[0] } else { mx[0] };
                let py = if corner[1] < 0.5 { mn[1] } else { mx[1] };
                let pz = if corner[2] < 0.5 { mn[2] } else { mx[2] };
                let (u, v) = face_uv(face.normal, px, py, pz);
                vertices.push(Vertex {
                    position: to_clip(Vec3::new(px, py, pz)),
                    color,
                    uv: [rect_uv[0] + u * rect_uv[2], rect_uv[1] + v * rect_uv[3]],
                    light: FULLBRIGHT,
                });
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// Append a flat item-icon's glint quad (clip-space `Vertex`es) for slot `rect`:
/// an axis-aligned quad covering the slot, textured with the item sprite's
/// block-atlas tile so the glint shader's alpha cutout masks the shimmer to the
/// sprite silhouette (the same `items/{name}` tile the world item glint uses).
pub fn append_flat_item_glint(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    item_id: i16,
    rect: UiRect,
    surface: (f32, f32),
    atlas: &AtlasUv,
) {
    let Some(name) = crate::texture::item_texture_name(item_id) else {
        return;
    };
    let name = format!("items/{name}");
    if atlas.is_missing_tile(Some(&name)) {
        return;
    }
    let r = atlas.tile_rect(Some(&name));
    // Slot rect → clip space (identity GUI camera, +y up like the cube path).
    let to_clip = |x: f32, y: f32| {
        [x / surface.0 * 2.0 - 1.0, 1.0 - y / surface.1 * 2.0, 0.0]
    };
    let (x0, y0) = (rect.x as f32, rect.y as f32);
    let (x1, y1) = ((rect.x + rect.width) as f32, (rect.y + rect.height) as f32);
    let corners = [(x0, y0), (x0, y1), (x1, y1), (x1, y0)];
    // UV order matching the corners (top-left, bottom-left, bottom-right, top-right).
    let uvs = [
        [r[0], r[1]],
        [r[0], r[1] + r[3]],
        [r[0] + r[2], r[1] + r[3]],
        [r[0] + r[2], r[1]],
    ];
    let base = vertices.len() as u32;
    for ((cx, cy), uv) in corners.iter().zip(uvs) {
        vertices.push(Vertex {
            position: to_clip(*cx, *cy),
            color: [1.0, 1.0, 1.0, 1.0],
            uv,
            light: FULLBRIGHT,
        });
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Vanilla element UV mapping within a tile (v grows downward), matching the
/// world mesher so partial boxes (slabs/stairs) sample the correct sub-region.
pub(crate) fn face_uv(normal: [i32; 3], px: f32, py: f32, pz: f32) -> (f32, f32) {
    match normal {
        [0, 1, 0] => (px, pz),
        [0, -1, 0] => (px, 1.0 - pz),
        [0, 0, 1] => (px, 1.0 - py),
        [0, 0, -1] => (1.0 - px, 1.0 - py),
        [1, 0, 0] => (1.0 - pz, 1.0 - py),
        _ => (pz, 1.0 - py),
    }
}

fn tint3(tint: Tint, biome: &BiomeColors) -> [f32; 3] {
    match tint {
        Tint::None => [1.0, 1.0, 1.0],
        Tint::Grass => biome.grass,
        Tint::Foliage => biome.foliage,
        // Vanilla water multiplier is 0xFFFFFF (white); the texture is already blue.
        Tint::Water => [1.0, 1.0, 1.0],
        Tint::Rgb(rgb) => rgb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextureAtlasImage;

    #[test]
    fn full_cube_blocks_are_3d_icons_but_air_is_not() {
        assert!(is_block_icon(1, 0).is_some()); // stone
        assert!(is_block_icon(0, 0).is_none()); // air
        assert!(is_block_icon(300, 0).is_none()); // not a block id
    }

    #[test]
    fn enchanted_flat_item_emits_a_clip_space_glint_quad() {
        let atlas = TextureAtlasImage::load_default().uv_table();
        let (mut v, mut i) = (Vec::new(), Vec::new());
        // A diamond sword (id 276) is a flat item icon; its glint is one quad
        // covering the slot, textured with the item's block-atlas sprite.
        append_flat_item_glint(&mut v, &mut i, 276, UiRect::new(100, 100, 32, 32), (800.0, 600.0), &atlas);
        assert_eq!(v.len(), 4);
        assert_eq!(i.len(), 6);
        for vert in &v {
            assert!((-1.0..=1.0).contains(&vert.position[0]), "{:?}", vert.position);
            assert!((-1.0..=1.0).contains(&vert.position[1]), "{:?}", vert.position);
        }
    }

    #[test]
    fn unknown_item_emits_no_glint() {
        let atlas = TextureAtlasImage::load_default().uv_table();
        let (mut v, mut i) = (Vec::new(), Vec::new());
        // An id with no item texture name produces nothing (no cutout to mask).
        append_flat_item_glint(&mut v, &mut i, 30000, UiRect::new(0, 0, 16, 16), (800.0, 600.0), &atlas);
        assert!(v.is_empty() && i.is_empty());
    }

    #[test]
    fn full_cube_emits_six_clip_space_faces() {
        let atlas = TextureAtlasImage::load_default().uv_table();
        let biome = BiomeColors::default();
        let (mut v, mut i) = (Vec::new(), Vec::new());
        append_block_icon(
            &mut v,
            &mut i,
            BlockState::new(1, 0),
            UiRect::new(100, 100, 32, 32),
            (800.0, 600.0),
            &atlas,
            &biome,
        );
        assert_eq!(v.len(), 24); // one cube box, 6 faces × 4
        assert_eq!(i.len(), 36);
        // Every vertex lands inside the wgpu clip cube.
        for vert in &v {
            assert!((-1.0..=1.0).contains(&vert.position[0]), "{:?}", vert.position);
            assert!((-1.0..=1.0).contains(&vert.position[1]), "{:?}", vert.position);
            assert!((0.0..=1.0).contains(&vert.position[2]), "{:?}", vert.position);
        }
    }
}
