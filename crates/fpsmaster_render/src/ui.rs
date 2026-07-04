use std::collections::HashMap;
use std::sync::Arc;

use image::RgbaImage;
use fpsmaster_core::{BlockFace, BlockState};

use crate::font;
use crate::texture::{AtlasUv, ItemAtlasImage, ATLAS_COLUMNS, TILE_SIZE};

/// A vanilla GUI texture the UI can blit sub-rectangles from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuiTexture {
    Widgets,
    Icons,
    /// gui/container/inventory.png — the survival inventory window background.
    Inventory,
    /// gui/options_background.png — the tiled dirt menu background.
    OptionsBackground,
    /// gui/title/minecraft.png — the vanilla MINECRAFT title logo (256×256 sheet,
    /// blitted from two 155×44 halves, matching vanilla `GuiMainMenu`).
    Title,
    /// gui/container/generic_54.png — the chest window (1..6 rows, blitted in
    /// two parts) and the fallback for unmodelled container types.
    Chest,
    /// gui/container/dispenser.png — the 3×3 dispenser/dropper window.
    Dispenser,
    /// gui/container/hopper.png — the 5-slot hopper window.
    Hopper,
    /// gui/container/furnace.png — the smelting window (with the flame/arrow
    /// progress sprites at fixed source rects).
    Furnace,
    /// gui/container/crafting_table.png — the 3×3 crafting window.
    CraftingTable,
    /// gui/container/brewing_stand.png — the brewing window.
    BrewingStand,
    /// gui/container/enchanting_table.png — the enchantment window.
    EnchantingTable,
    /// gui/container/anvil.png — the anvil repair/rename window.
    Anvil,
    /// gui/container/beacon.png — the beacon window (larger than 176×166).
    Beacon,
    /// gui/container/villager.png — the villager trading window.
    Villager,
}

/// Loaded vanilla GUI textures (hotbar widget, status icons, inventory window)
/// plus a CPU copy of the block atlas for drawing block item thumbnails. Missing
/// textures simply make the corresponding blits no-ops.
#[derive(Debug, Default)]
pub struct GuiAtlas {
    pub widgets: Option<RgbaImage>,
    pub icons: Option<RgbaImage>,
    pub inventory: Option<RgbaImage>,
    pub options_background: Option<RgbaImage>,
    pub title: Option<RgbaImage>,
    pub chest: Option<RgbaImage>,
    pub dispenser: Option<RgbaImage>,
    pub hopper: Option<RgbaImage>,
    pub furnace: Option<RgbaImage>,
    pub crafting_table: Option<RgbaImage>,
    pub brewing_stand: Option<RgbaImage>,
    pub enchanting_table: Option<RgbaImage>,
    pub anvil: Option<RgbaImage>,
    pub beacon: Option<RgbaImage>,
    pub villager: Option<RgbaImage>,
    /// The 16×16-tile block atlas, used as item-icon source for block items.
    blocks: Option<RgbaImage>,
    block_uv: AtlasUv,
    /// Item thumbnails (tools/food/etc.) for non-block item ids.
    items: ItemAtlasImage,
}

impl GuiAtlas {
    /// `blocks`/`block_uv` are the renderer's already-built block atlas (image +
    /// name→tile map), reused here for block item thumbnails.
    pub fn load(blocks: Option<RgbaImage>, block_uv: AtlasUv) -> Self {
        Self {
            widgets: crate::texture::load_gui_image("widgets"),
            icons: crate::texture::load_gui_image("icons"),
            inventory: crate::texture::load_gui_image("container/inventory"),
            options_background: crate::texture::load_gui_image("options_background"),
            // Vanilla MINECRAFT logo from the active assets (gui/title/minecraft.png).
            title: crate::texture::load_gui_image("title/minecraft"),
            chest: crate::texture::load_gui_image("container/generic_54"),
            dispenser: crate::texture::load_gui_image("container/dispenser"),
            hopper: crate::texture::load_gui_image("container/hopper"),
            furnace: crate::texture::load_gui_image("container/furnace"),
            crafting_table: crate::texture::load_gui_image("container/crafting_table"),
            brewing_stand: crate::texture::load_gui_image("container/brewing_stand"),
            enchanting_table: crate::texture::load_gui_image("container/enchanting_table"),
            anvil: crate::texture::load_gui_image("container/anvil"),
            beacon: crate::texture::load_gui_image("container/beacon"),
            villager: crate::texture::load_gui_image("container/villager"),
            blocks,
            block_uv,
            items: ItemAtlasImage::load_default(),
        }
    }

    fn get(&self, texture: GuiTexture) -> Option<&RgbaImage> {
        match texture {
            GuiTexture::Widgets => self.widgets.as_ref(),
            GuiTexture::Icons => self.icons.as_ref(),
            GuiTexture::Inventory => self.inventory.as_ref(),
            GuiTexture::OptionsBackground => self.options_background.as_ref(),
            GuiTexture::Title => self.title.as_ref(),
            GuiTexture::Chest => self.chest.as_ref(),
            GuiTexture::Dispenser => self.dispenser.as_ref(),
            GuiTexture::Hopper => self.hopper.as_ref(),
            GuiTexture::Furnace => self.furnace.as_ref(),
            GuiTexture::CraftingTable => self.crafting_table.as_ref(),
            GuiTexture::BrewingStand => self.brewing_stand.as_ref(),
            GuiTexture::EnchantingTable => self.enchanting_table.as_ref(),
            GuiTexture::Anvil => self.anvil.as_ref(),
            GuiTexture::Beacon => self.beacon.as_ref(),
            GuiTexture::Villager => self.villager.as_ref(),
        }
    }

    /// The block-atlas source pixel rect for a block item id's top-face texture,
    /// or None when the id isn't a known (non-air) block.
    fn block_tile(&self, item_id: i16) -> Option<(u32, u32)> {
        if !(0..256).contains(&item_id) {
            return None;
        }
        let block = BlockState::new(item_id as u16, 0);
        if block.is_air() {
            return None;
        }
        let name = block.texture_name(BlockFace::Top);
        if self.block_uv.is_missing_tile(name) {
            crate::texture::warn_missing_tile(block.id, block.meta, "its item icon", name);
        }
        let index = self.block_uv.tile_index(name);
        Some((
            index % ATLAS_COLUMNS * TILE_SIZE,
            index / ATLAS_COLUMNS * TILE_SIZE,
        ))
    }

    /// The loaded RGBA source for a GUI texture (for GPU upload / UV sizing), or
    /// None when the asset is missing.
    pub fn image(&self, texture: GuiTexture) -> Option<&RgbaImage> {
        self.get(texture)
    }

    /// The block atlas image (item-icon source for block items), if loaded.
    pub fn blocks_image(&self) -> Option<&RgbaImage> {
        self.blocks.as_ref()
    }

    /// The item atlas image (thumbnail source for non-block items), if loaded.
    pub fn items_image(&self) -> Option<&RgbaImage> {
        self.items.image()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl UiRect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x as f64
            && y >= self.y as f64
            && x < (self.x + self.width) as f64
            && y < (self.y + self.height) as f64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl UiColor {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Rect {
        rect: UiRect,
        color: UiColor,
    },
    Text {
        x: i32,
        y: i32,
        scale: i32,
        color: UiColor,
        text: String,
        /// Draw the vanilla drop shadow (+1,+1 font px, colors quartered).
        shadow: bool,
    },
    /// Blit a sub-rectangle of a GUI texture, scaled to `dst`.
    Image {
        dst: UiRect,
        texture: GuiTexture,
        sx: u32,
        sy: u32,
        sw: u32,
        sh: u32,
    },
    /// Draw an item thumbnail scaled to `dst`: the block atlas tile for block
    /// items, or a deterministic tint swatch for other item ids.
    ItemIcon {
        dst: UiRect,
        item_id: i16,
    },
    /// Tile a GUI texture across `dst` at `tile_px` screen pixels per repeat,
    /// multiplied by `tint` (the vanilla dirt background uses gray 64).
    TiledImage {
        dst: UiRect,
        texture: GuiTexture,
        tile_px: i32,
        tint: UiColor,
    },
    /// Blit a free-standing RGBA image (e.g. a downloaded server favicon, or a
    /// mod-registered texture), nearest-scaled and alpha-composited into `dst`.
    /// The image is shared via `Arc` so frame-diffing stays a cheap pointer
    /// compare. `src` is an optional `(sx,sy,sw,sh)` source sub-rect.
    RawImage {
        dst: UiRect,
        image: std::sync::Arc<RgbaImage>,
        src: Option<(u32, u32, u32, u32)>,
    },
    /// A straight line from `(x0,y0)` to `(x1,y1)`, `width` px thick, alpha-
    /// composited. Coordinates are pre-scale (divided by `pixel_scale`).
    Line {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: UiColor,
        width: i32,
    },
    /// A vertical gradient (vanilla `drawGradientRect`): `top` at the top edge
    /// lerped to `bottom` at the bottom edge, alpha-composited over the buffer.
    /// Drives the item tooltip's dark fill and purple border, the menu list
    /// shadow lines, and the title-screen overlays.
    GradientRect {
        rect: UiRect,
        top: UiColor,
        bottom: UiColor,
    },
}

/// A block rendered as a real 3D cube into its slot rect (the GPU cube pass),
/// instead of a flat top-face thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuiBlockItem {
    pub dst: UiRect,
    pub block_id: u16,
    pub meta: u8,
}

/// An enchanted item icon to overlay with the scrolling glint: a 3D block-icon
/// cube (`block` set) shimmers over its cube geometry, anything else as a flat
/// quad over its slot rect. Built into a clip-space glint mesh by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuiGlintItem {
    pub dst: UiRect,
    pub item_id: i16,
    /// `Some((block_id, meta))` when the icon is a 3D block cube.
    pub block: Option<(u16, u8)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiFrame {
    /// Background layer: drawn under the 3D block icons.
    commands: Vec<UiCommand>,
    /// Foreground layer: stack counts, hover highlight and the carried stack,
    /// drawn over the 3D block icons.
    overlay: Vec<UiCommand>,
    /// 3D block icons, rendered by the GPU cube pass between the two layers.
    block_items: Vec<GuiBlockItem>,
    /// Enchanted item icons to overlay with the scrolling glint (drawn over the
    /// icons in the UI pass, additively, like the held/world item glint).
    glint_items: Vec<GuiGlintItem>,
    /// Crosshair layer: a single sprite drawn on its own GPU pass with the
    /// vanilla inversion blend (`GL_ONE_MINUS_DST_COLOR`/`GL_ONE_MINUS_SRC_COLOR`)
    /// so it shows as the inverse of the 3D scene behind it.
    crosshair: Vec<UiCommand>,
}

impl UiFrame {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
            && self.overlay.is_empty()
            && self.block_items.is_empty()
            && self.glint_items.is_empty()
            && self.crosshair.is_empty()
    }

    /// Background (under-cube) and foreground (over-cube) command layers.
    pub fn back_commands(&self) -> &[UiCommand] {
        &self.commands
    }

    pub fn overlay_commands(&self) -> &[UiCommand] {
        &self.overlay
    }

    pub fn block_items(&self) -> &[GuiBlockItem] {
        &self.block_items
    }

    pub fn glint_items(&self) -> &[GuiGlintItem] {
        &self.glint_items
    }

    pub fn crosshair_commands(&self) -> &[UiCommand] {
        &self.crosshair
    }

    /// Queue the vanilla crosshair: the 16×16 sprite at (0,0) of `gui/icons.png`,
    /// drawn at `dst`. It goes on its own inversion-blend GPU layer, matching
    /// vanilla `GuiIngame` (`drawTexturedModalRect(.., 0, 0, 16, 16)` under the
    /// `ONE_MINUS_DST_COLOR`/`ONE_MINUS_SRC_COLOR` blend).
    pub fn crosshair(&mut self, dst: UiRect) {
        self.crosshair.push(UiCommand::Image {
            dst,
            texture: GuiTexture::Icons,
            sx: 0,
            sy: 0,
            sw: 16,
            sh: 16,
        });
    }

    /// Queue an enchanted item icon's glint overlay. `block` carries the cube's
    /// `(block_id, meta)` when the icon is a 3D block cube (so the glint hugs the
    /// cube faces), otherwise the flat slot rect is used. Drawn additively over
    /// the icon in the UI pass, matching the held/world item glint.
    pub fn glint_item(&mut self, dst: UiRect, item_id: i16, block: Option<(u16, u8)>) {
        self.glint_items.push(GuiGlintItem { dst, item_id, block });
    }

    /// Queue a 3D block icon (the GPU cube pass draws it; counts/highlights go
    /// to the overlay layer so they stay on top).
    pub fn block_item(&mut self, dst: UiRect, block_id: u16, meta: u8) {
        self.block_items.push(GuiBlockItem {
            dst,
            block_id,
            meta,
        });
    }

    /// Overlay-layer variants (drawn over the 3D block icons).
    pub fn overlay_rect(&mut self, rect: UiRect, color: UiColor) {
        self.overlay.push(UiCommand::Rect { rect, color });
    }

    /// A vertical gradient on the overlay layer (item tooltip box/border).
    pub fn overlay_gradient_rect(&mut self, rect: UiRect, top: UiColor, bottom: UiColor) {
        self.overlay.push(UiCommand::GradientRect { rect, top, bottom });
    }

    pub fn overlay_item_icon(&mut self, dst: UiRect, item_id: i16) {
        self.overlay.push(UiCommand::ItemIcon { dst, item_id });
    }

    pub fn overlay_text_shadowed(
        &mut self,
        x: i32,
        y: i32,
        scale: i32,
        color: UiColor,
        text: impl Into<String>,
    ) {
        self.overlay.push(UiCommand::Text {
            x,
            y,
            scale: scale.max(1),
            color,
            text: text.into(),
            shadow: true,
        });
    }

    pub fn rect(&mut self, rect: UiRect, color: UiColor) {
        self.commands.push(UiCommand::Rect { rect, color });
    }

    pub fn text(&mut self, x: i32, y: i32, scale: i32, color: UiColor, text: impl Into<String>) {
        self.commands.push(UiCommand::Text {
            x,
            y,
            scale: scale.max(1),
            color,
            text: text.into(),
            shadow: false,
        });
    }

    /// Text with the vanilla drop shadow (chat, action bar, item counts…).
    pub fn text_shadowed(
        &mut self,
        x: i32,
        y: i32,
        scale: i32,
        color: UiColor,
        text: impl Into<String>,
    ) {
        self.commands.push(UiCommand::Text {
            x,
            y,
            scale: scale.max(1),
            color,
            text: text.into(),
            shadow: true,
        });
    }

    pub fn text_centered(
        &mut self,
        rect: UiRect,
        scale: i32,
        color: UiColor,
        text: impl Into<String>,
    ) {
        let text = text.into();
        let scale = scale.max(1);
        let x = rect.x + (rect.width - text_width(&text, scale)) / 2;
        let y = rect.y + (rect.height - text_height(scale)) / 2;
        self.text(x, y, scale, color, text);
    }

    /// Blit `src` rect of `texture` scaled into `dst`.
    pub fn image(&mut self, dst: UiRect, texture: GuiTexture, sx: u32, sy: u32, sw: u32, sh: u32) {
        self.commands.push(UiCommand::Image {
            dst,
            texture,
            sx,
            sy,
            sw,
            sh,
        });
    }

    /// Draw an item thumbnail (block-atlas tile, or tint swatch) scaled to `dst`.
    pub fn item_icon(&mut self, dst: UiRect, item_id: i16) {
        self.commands.push(UiCommand::ItemIcon { dst, item_id });
    }

    /// Vertical gradient from `top` to `bottom` on the background layer
    /// (menu list shadow lines, title-screen overlays).
    pub fn gradient_rect(&mut self, rect: UiRect, top: UiColor, bottom: UiColor) {
        self.commands.push(UiCommand::GradientRect { rect, top, bottom });
    }

    /// Tile `texture` across `dst` (`tile_px` screen px per repeat) with `tint`.
    pub fn tiled_image(&mut self, dst: UiRect, texture: GuiTexture, tile_px: i32, tint: UiColor) {
        self.commands.push(UiCommand::TiledImage {
            dst,
            texture,
            tile_px: tile_px.max(1),
            tint,
        });
    }

    /// Blit a free-standing RGBA image (server favicon, mod texture) into `dst`.
    pub fn raw_image(&mut self, dst: UiRect, image: std::sync::Arc<RgbaImage>) {
        self.commands.push(UiCommand::RawImage {
            dst,
            image,
            src: None,
        });
    }

    /// Blit a sub-rect `(sx,sy,sw,sh)` of a free-standing RGBA image into `dst`.
    pub fn raw_image_src(
        &mut self,
        dst: UiRect,
        image: std::sync::Arc<RgbaImage>,
        src: (u32, u32, u32, u32),
    ) {
        self.commands.push(UiCommand::RawImage {
            dst,
            image,
            src: Some(src),
        });
    }

    /// A straight line from `(x0,y0)` to `(x1,y1)`, `width` px thick.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: UiColor, width: i32) {
        self.commands.push(UiCommand::Line {
            x0,
            y0,
            x1,
            y1,
            color,
            width: width.max(1),
        });
    }

    /// Tessellate one UI command layer into GPU quads for the batched UI pass.
    /// Commands are emitted in order — one batch per contiguous run sharing a
    /// texture — so painter's ordering is preserved exactly. Coordinates are in
    /// physical screen pixels (converted to clip space against `screen_w/h`); the
    /// nearest sampler reproduces the vanilla chunky upscale that the old
    /// downscaled CPU rasterize used to bake in.
    pub fn tessellate(
        commands: &[UiCommand],
        screen_w: u32,
        screen_h: u32,
        gui: &GuiAtlas,
    ) -> UiGeometry {
        let mut t = Tess::new(screen_w, screen_h, gui);
        for command in commands {
            t.command(command);
        }
        t.geo
    }
}

/// A UI quad vertex: clip-space position, atlas UV, and an sRGB-encoded colour
/// (rgb linearized in the shader; alpha stays linear).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct UiVertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

impl UiVertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];

    pub fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// Which texture a [`UiBatch`] samples. The renderer resolves each to a bound
/// atlas (uploading lazily); `White` is a shared 1×1 texel for solid fills.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UiTextureId {
    White,
    FontAscii,
    FontPage(u8),
    Gui(GuiTexture),
    Blocks,
    Items,
    /// Index into [`UiGeometry::raw_images`] (a favicon / mod-registered texture).
    Raw(usize),
}

/// A contiguous run of quad vertices sharing one texture — drawn as one call.
pub struct UiBatch {
    pub texture: UiTextureId,
    pub first_vertex: u32,
    pub vertex_count: u32,
}

/// The tessellated geometry for one UI layer.
#[derive(Default)]
pub struct UiGeometry {
    pub vertices: Vec<UiVertex>,
    pub batches: Vec<UiBatch>,
    /// Free-standing images referenced by `UiTextureId::Raw(i)` in this layer.
    pub raw_images: Vec<Arc<RgbaImage>>,
}

/// White vertex tint (leaves the sampled texel unchanged).
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// UiColor → sRGB-encoded 0..1 (the shader linearizes rgb; alpha stays linear).
fn col(c: UiColor) -> [f32; 4] {
    [
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        c.a as f32 / 255.0,
    ]
}

/// Normalized UV rect for a source pixel sub-rect of an `iw×ih` atlas.
fn src_uv(sx: u32, sy: u32, sw: u32, sh: u32, iw: u32, ih: u32) -> [f32; 4] {
    let iw = iw.max(1) as f32;
    let ih = ih.max(1) as f32;
    [
        sx as f32 / iw,
        sy as f32 / ih,
        (sx + sw) as f32 / iw,
        (sy + sh) as f32 / ih,
    ]
}

/// The quad tessellator: walks a command layer, appending vertices and starting
/// a new batch whenever the sampled texture changes.
struct Tess<'a> {
    geo: UiGeometry,
    raw_index: HashMap<usize, usize>,
    sw: f32,
    sh: f32,
    gui: &'a GuiAtlas,
}

impl<'a> Tess<'a> {
    fn new(w: u32, h: u32, gui: &'a GuiAtlas) -> Self {
        Self {
            geo: UiGeometry::default(),
            raw_index: HashMap::new(),
            sw: w.max(1) as f32,
            sh: h.max(1) as f32,
            gui,
        }
    }

    /// Append a quad (4 corners of `(screen_px, uv, color)`, ordered TL, TR, BR,
    /// BL) as two triangles, extending the current batch or starting a new one.
    fn push(&mut self, tex: UiTextureId, corners: [([f32; 2], [f32; 2], [f32; 4]); 4]) {
        let (sw, sh) = (self.sw, self.sh);
        let vert = |c: &([f32; 2], [f32; 2], [f32; 4])| UiVertex {
            pos: [c.0[0] / sw * 2.0 - 1.0, 1.0 - c.0[1] / sh * 2.0],
            uv: c.1,
            color: c.2,
        };
        let (tl, tr, br, bl) = (
            vert(&corners[0]),
            vert(&corners[1]),
            vert(&corners[2]),
            vert(&corners[3]),
        );
        if self.geo.batches.last().is_none_or(|b| b.texture != tex) {
            let first_vertex = self.geo.vertices.len() as u32;
            self.geo.batches.push(UiBatch {
                texture: tex,
                first_vertex,
                vertex_count: 0,
            });
        }
        self.geo.vertices.extend_from_slice(&[tl, tr, br, tl, br, bl]);
        self.geo.batches.last_mut().unwrap().vertex_count += 6;
    }

    /// An axis-aligned quad with a single colour and UV rect `[u0,v0,u1,v1]`.
    #[allow(clippy::too_many_arguments)]
    fn rect(
        &mut self,
        tex: UiTextureId,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        uv: [f32; 4],
        color: [f32; 4],
    ) {
        self.push(
            tex,
            [
                ([x, y], [uv[0], uv[1]], color),
                ([x + w, y], [uv[2], uv[1]], color),
                ([x + w, y + h], [uv[2], uv[3]], color),
                ([x, y + h], [uv[0], uv[3]], color),
            ],
        );
    }

    /// A solid (white-texel) fill.
    fn solid(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        self.rect(UiTextureId::White, x, y, w, h, [0.5, 0.5, 0.5, 0.5], color);
    }

    /// A single glyph placement from [`font::Font::layout`], with italic shear.
    fn font_quad(&mut self, g: crate::font::FontQuad) {
        let (tex, aw, ah) = match g.tex {
            crate::font::FontTex::Ascii => (UiTextureId::FontAscii, 128.0, 128.0),
            crate::font::FontTex::Page(p) => (UiTextureId::FontPage(p), 256.0, 256.0),
        };
        let (u0, v0) = (g.sx as f32 / aw, g.sy as f32 / ah);
        let (u1, v1) = ((g.sx + g.sw) as f32 / aw, (g.sy + g.sh) as f32 / ah);
        let color = [
            g.color[0] as f32 / 255.0,
            g.color[1] as f32 / 255.0,
            g.color[2] as f32 / 255.0,
            g.color[3] as f32 / 255.0,
        ];
        let (x, y, w, h, s) = (g.x as f32, g.y as f32, g.w as f32, g.h as f32, g.shear as f32);
        // Italic: top edge shifted +s, bottom edge -s (linear shear).
        self.push(
            tex,
            [
                ([x + s, y], [u0, v0], color),
                ([x + w + s, y], [u1, v0], color),
                ([x + w - s, y + h], [u1, v1], color),
                ([x - s, y + h], [u0, v1], color),
            ],
        );
    }

    /// Intern a free-standing image by `Arc` identity, returning its `Raw` index.
    fn raw_id(&mut self, image: &Arc<RgbaImage>) -> usize {
        let key = Arc::as_ptr(image) as usize;
        if let Some(&i) = self.raw_index.get(&key) {
            return i;
        }
        let i = self.geo.raw_images.len();
        self.geo.raw_images.push(image.clone());
        self.raw_index.insert(key, i);
        i
    }

    fn command(&mut self, cmd: &UiCommand) {
        match cmd {
            UiCommand::Rect { rect, color } => {
                self.solid(
                    rect.x as f32,
                    rect.y as f32,
                    rect.width as f32,
                    rect.height as f32,
                    col(*color),
                );
            }
            UiCommand::Text {
                x,
                y,
                scale,
                color,
                text,
                shadow,
            } => {
                let base = [color.r, color.g, color.b, color.a];
                font::font().layout(*x, *y, *scale, base, *shadow, text, &mut |d| match d {
                    crate::font::FontDraw::Glyph(g) => self.font_quad(g),
                    crate::font::FontDraw::Solid { x, y, w, h, color } => self.solid(
                        x as f32,
                        y as f32,
                        w as f32,
                        h as f32,
                        [
                            color[0] as f32 / 255.0,
                            color[1] as f32 / 255.0,
                            color[2] as f32 / 255.0,
                            color[3] as f32 / 255.0,
                        ],
                    ),
                });
            }
            UiCommand::Image {
                dst,
                texture,
                sx,
                sy,
                sw,
                sh,
            } => {
                if let Some((iw, ih)) = self.gui.image(*texture).map(|i| i.dimensions()) {
                    let uv = src_uv(*sx, *sy, *sw, *sh, iw, ih);
                    self.rect(
                        UiTextureId::Gui(*texture),
                        dst.x as f32,
                        dst.y as f32,
                        dst.width as f32,
                        dst.height as f32,
                        uv,
                        WHITE,
                    );
                }
            }
            UiCommand::TiledImage {
                dst,
                texture,
                tile_px,
                tint,
            } => {
                let tile = (*tile_px).max(1);
                if self.gui.image(*texture).is_some() {
                    let tint_c = col(*tint);
                    let mut oy = 0;
                    while oy < dst.height {
                        let th = tile.min(dst.height - oy);
                        let mut ox = 0;
                        while ox < dst.width {
                            let tw = tile.min(dst.width - ox);
                            let uv = [0.0, 0.0, tw as f32 / tile as f32, th as f32 / tile as f32];
                            self.rect(
                                UiTextureId::Gui(*texture),
                                (dst.x + ox) as f32,
                                (dst.y + oy) as f32,
                                tw as f32,
                                th as f32,
                                uv,
                                tint_c,
                            );
                            ox += tile;
                        }
                        oy += tile;
                    }
                } else {
                    // Missing texture: a flat dark fill keeps menus readable.
                    self.solid(
                        dst.x as f32,
                        dst.y as f32,
                        dst.width as f32,
                        dst.height as f32,
                        col(UiColor::rgba(28, 22, 18, 255)),
                    );
                }
            }
            UiCommand::RawImage { dst, image, src } => {
                let (iw, ih) = image.dimensions();
                let (sx, sy, sw, sh) = src.unwrap_or((0, 0, iw, ih));
                let uv = src_uv(sx, sy, sw, sh, iw, ih);
                let id = self.raw_id(image);
                self.rect(
                    UiTextureId::Raw(id),
                    dst.x as f32,
                    dst.y as f32,
                    dst.width as f32,
                    dst.height as f32,
                    uv,
                    WHITE,
                );
            }
            UiCommand::Line {
                x0,
                y0,
                x1,
                y1,
                color,
                width,
            } => {
                let (p0, p1) = ([*x0 as f32, *y0 as f32], [*x1 as f32, *y1 as f32]);
                let (dx, dy) = (p1[0] - p0[0], p1[1] - p0[1]);
                let len = (dx * dx + dy * dy).sqrt().max(1e-3);
                let hw = (*width).max(1) as f32 / 2.0;
                let (ox, oy) = (-dy / len * hw, dx / len * hw);
                let c = col(*color);
                self.push(
                    UiTextureId::White,
                    [
                        ([p0[0] + ox, p0[1] + oy], [0.5, 0.5], c),
                        ([p1[0] + ox, p1[1] + oy], [0.5, 0.5], c),
                        ([p1[0] - ox, p1[1] - oy], [0.5, 0.5], c),
                        ([p0[0] - ox, p0[1] - oy], [0.5, 0.5], c),
                    ],
                );
            }
            UiCommand::ItemIcon { dst, item_id } => {
                let (x, y, w, h) = (
                    dst.x as f32,
                    dst.y as f32,
                    dst.width as f32,
                    dst.height as f32,
                );
                if let (Some((sx, sy)), Some((bw, bh))) = (
                    self.gui.block_tile(*item_id),
                    self.gui.blocks.as_ref().map(|b| b.dimensions()),
                ) {
                    let uv = src_uv(sx, sy, TILE_SIZE, TILE_SIZE, bw, bh);
                    self.rect(UiTextureId::Blocks, x, y, w, h, uv, WHITE);
                } else if let (Some((sx, sy)), Some((iw, ih))) = (
                    self.gui.items.tile_for_id(*item_id),
                    self.gui.items.image().map(|i| i.dimensions()),
                ) {
                    let uv = src_uv(sx, sy, TILE_SIZE, TILE_SIZE, iw, ih);
                    self.rect(UiTextureId::Items, x, y, w, h, uv, WHITE);
                } else {
                    self.solid(x, y, w, h, col(item_swatch_color(*item_id)));
                }
            }
            UiCommand::GradientRect { rect, top, bottom } => {
                let (x, y, w, h) = (
                    rect.x as f32,
                    rect.y as f32,
                    rect.width as f32,
                    rect.height as f32,
                );
                let (ct, cb) = (col(*top), col(*bottom));
                self.push(
                    UiTextureId::White,
                    [
                        ([x, y], [0.5, 0.5], ct),
                        ([x + w, y], [0.5, 0.5], ct),
                        ([x + w, y + h], [0.5, 0.5], cb),
                        ([x, y + h], [0.5, 0.5], cb),
                    ],
                );
            }
        }
    }
}

/// The GUI pixel scale for a window of `width`×`height` (vanilla
/// `ScaledResolution` auto gui-scale with `guiScale=0`).
pub fn gui_pixel_scale(width: u32, height: u32) -> u32 {
    gui_pixel_scale_capped(width, height, 0)
}

/// The GUI pixel scale for a window of `width`×`height` under a user GUI-scale
/// preference, matching vanilla `ScaledResolution`: `gui_scale` `0` means "Auto"
/// (the largest scale that still fits a 320×240 layout), while `1..` is a fixed
/// upper cap (Small/Normal/Large/…). The window-fit conditions still bound the
/// result, so a fixed cap never forces a UI too large for the window — it only
/// ever holds the scale *lower* than Auto would pick.
pub fn gui_pixel_scale_capped(width: u32, height: u32, gui_scale: u32) -> u32 {
    let cap = if gui_scale == 0 { 1000 } else { gui_scale };
    let mut scale = 1u32;
    while scale < cap
        && width / (scale + 1) >= 320
        && height / (scale + 1) >= 240
    {
        scale += 1;
    }
    scale.max(1)
}

/// Width of §-coded text in screen px (vanilla per-character advances).
pub fn text_width(text: &str, scale: i32) -> i32 {
    font::font().text_width(text, scale)
}

pub fn text_height(scale: i32) -> i32 {
    8 * scale.max(1)
}

/// Deterministic tint for a non-block item id so it stays distinguishable.
fn item_swatch_color(id: i16) -> UiColor {
    let id = id as u32;
    UiColor::rgba(
        80 + (id.wrapping_mul(73) % 160) as u8,
        80 + (id.wrapping_mul(151) % 160) as u8,
        80 + (id.wrapping_mul(199) % 160) as u8,
        255,
    )
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_scale_auto_matches_uncapped_and_cap_bounds_it() {
        // 1080p: Auto (0) resolves to 4, and the plain helper agrees.
        assert_eq!(gui_pixel_scale_capped(1920, 1080, 0), 4);
        assert_eq!(gui_pixel_scale(1920, 1080), 4);
        // A fixed cap only ever holds the scale lower than Auto.
        assert_eq!(gui_pixel_scale_capped(1920, 1080, 1), 1);
        assert_eq!(gui_pixel_scale_capped(1920, 1080, 2), 2);
        // 720p only fits 3, so a cap of 4 still resolves to 3 (never oversized).
        assert_eq!(gui_pixel_scale_capped(1280, 720, 0), 3);
        assert_eq!(gui_pixel_scale_capped(1280, 720, 4), 3);
        // Tiny window: scale is at least 1 whatever the cap.
        assert_eq!(gui_pixel_scale_capped(200, 200, 0), 1);
        assert_eq!(gui_pixel_scale_capped(200, 200, 3), 1);
    }

    #[test]
    fn gradient_rect_emits_one_white_quad_with_top_and_bottom_colors() {
        let mut frame = UiFrame::new();
        frame.overlay_gradient_rect(
            UiRect::new(0, 0, 2, 4),
            UiColor::rgba(80, 0, 255, 80),
            UiColor::rgba(40, 0, 127, 80),
        );
        let geo = UiFrame::tessellate(frame.overlay_commands(), 100, 100, &GuiAtlas::default());
        // One gradient → one white-texel batch of six vertices (two triangles).
        assert_eq!(geo.batches.len(), 1);
        assert!(matches!(geo.batches[0].texture, UiTextureId::White));
        assert_eq!(geo.batches[0].vertex_count, 6);
        assert_eq!(geo.vertices.len(), 6);
        // Colours are sRGB-encoded 0..1: top corners carry `top`, bottom `bottom`.
        let top = geo.vertices[0].color; // TL
        let bottom = geo.vertices[5].color; // BL
        assert!((top[2] - 1.0).abs() < 1e-6, "top blue is 255/255");
        assert!(bottom[2] < top[2], "blue decreases downward");
        assert!((top[3] - 80.0 / 255.0).abs() < 1e-6, "alpha carried through");
    }

    #[test]
    fn rect_maps_pixels_to_clip_space() {
        let mut frame = UiFrame::new();
        // A rect covering the whole 200×100 screen maps to the full NDC quad.
        frame.rect(UiRect::new(0, 0, 200, 100), UiColor::rgba(10, 20, 30, 255));
        let geo = UiFrame::tessellate(frame.back_commands(), 200, 100, &GuiAtlas::default());
        assert_eq!(geo.batches.len(), 1);
        let tl = geo.vertices[0].pos; // top-left → (-1, 1)
        assert!((tl[0] + 1.0).abs() < 1e-6 && (tl[1] - 1.0).abs() < 1e-6);
        let br = geo.vertices[2].pos; // bottom-right → (1, -1)
        assert!((br[0] - 1.0).abs() < 1e-6 && (br[1] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn painter_order_splits_batches_by_texture() {
        // Background rect, then shadowed text, then another rect: rects use the
        // white texel and text uses the font atlas, so the batch list alternates
        // while preserving submission order (painter's algorithm).
        let mut frame = UiFrame::new();
        frame.rect(UiRect::new(0, 0, 10, 10), UiColor::rgba(0, 0, 0, 128));
        frame.text_shadowed(1, 1, 1, UiColor::rgba(255, 255, 255, 255), "Hi");
        frame.rect(UiRect::new(0, 0, 10, 10), UiColor::rgba(0, 0, 0, 128));
        let geo = UiFrame::tessellate(frame.back_commands(), 100, 100, &GuiAtlas::default());
        assert!(geo.batches.len() >= 3, "rect / text / rect do not merge");
        assert!(matches!(geo.batches.first().unwrap().texture, UiTextureId::White));
        assert!(matches!(geo.batches.last().unwrap().texture, UiTextureId::White));
        assert!(
            geo.batches
                .iter()
                .any(|b| matches!(b.texture, UiTextureId::FontAscii)),
            "text should emit ascii glyph quads"
        );
    }
}
