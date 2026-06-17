use image::RgbaImage;
use recraft_core::{BlockFace, BlockState};

use crate::font;
use crate::texture::{AtlasUv, ItemAtlasImage, ATLAS_COLUMNS, TILE_SIZE};

/// A vanilla GUI texture the UI can blit sub-rectangles from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiTexture {
    Widgets,
    Icons,
    /// gui/container/inventory.png — the survival inventory window background.
    Inventory,
    /// gui/options_background.png — the tiled dirt menu background.
    OptionsBackground,
    /// Custom single-image MINECRAFT title logo bundled with the binary.
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
            // Custom single-image MINECRAFT logo bundled with the binary.
            title: image::load_from_memory(include_bytes!("embedded/title_logo.png"))
                .ok()
                .map(|img| img.to_rgba8()),
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
    /// Blit a free-standing RGBA image (e.g. a downloaded server favicon),
    /// nearest-scaled and alpha-composited into `dst`. The image is shared via
    /// `Arc` so frame-diffing stays a cheap pointer compare.
    RawImage {
        dst: UiRect,
        image: std::sync::Arc<RgbaImage>,
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

    /// Blit a free-standing RGBA image (server favicon) into `dst`.
    pub fn raw_image(&mut self, dst: UiRect, image: std::sync::Arc<RgbaImage>) {
        self.commands.push(UiCommand::RawImage { dst, image });
    }

    /// Rasterize into a buffer downscaled by `pixel_scale` (the GUI pixel
    /// scale): command coordinates are divided by it, so the CPU rasterizes at
    /// GUI resolution and the GPU upscales nearest-neighbour — the vanilla
    /// chunky look at a fraction of the per-frame cost.
    pub fn rasterize(
        commands: &[UiCommand],
        width: u32,
        height: u32,
        pixel_scale: u32,
        gui: &GuiAtlas,
    ) -> Vec<u8> {
        let s = pixel_scale.max(1) as i32;
        let mut pixels = vec![0; width as usize * height as usize * 4];
        for command in commands {
            match command {
                UiCommand::Rect { rect, color } => {
                    fill_rect(&mut pixels, width, height, scale_rect(*rect, s), *color)
                }
                UiCommand::Text {
                    x,
                    y,
                    scale,
                    color,
                    text,
                    shadow,
                } => {
                    let f = font::font();
                    let rgba = [color.r, color.g, color.b, color.a];
                    let glyph_scale = (*scale / s).max(1);
                    let (tx, ty) = (*x / s, *y / s);
                    if *shadow {
                        f.draw(
                            &mut pixels,
                            width,
                            height,
                            tx + glyph_scale,
                            ty + glyph_scale,
                            glyph_scale,
                            rgba,
                            true,
                            text,
                        );
                    }
                    f.draw(
                        &mut pixels,
                        width,
                        height,
                        tx,
                        ty,
                        glyph_scale,
                        rgba,
                        false,
                        text,
                    );
                }
                UiCommand::Image {
                    dst,
                    texture,
                    sx,
                    sy,
                    sw,
                    sh,
                } => {
                    if let Some(src) = gui.get(*texture) {
                        let dst = scale_rect(*dst, s);
                        blit_image(&mut pixels, width, height, dst, src, *sx, *sy, *sw, *sh);
                    }
                }
                UiCommand::TiledImage {
                    dst,
                    texture,
                    tile_px,
                    tint,
                } => {
                    let dst = scale_rect(*dst, s);
                    if let Some(src) = gui.get(*texture) {
                        tile_image(&mut pixels, width, height, dst, src, (*tile_px / s).max(1), *tint);
                    } else {
                        // Missing texture: a flat dark fill keeps menus readable.
                        fill_rect(&mut pixels, width, height, dst, UiColor::rgba(28, 22, 18, 255));
                    }
                }
                UiCommand::RawImage { dst, image } => {
                    let dst = scale_rect(*dst, s);
                    let (iw, ih) = image.dimensions();
                    blit_image(&mut pixels, width, height, dst, image, 0, 0, iw, ih);
                }
                UiCommand::ItemIcon { dst, item_id } => {
                    let dst = &scale_rect(*dst, s);
                    if let (Some((sx, sy)), Some(blocks)) = (gui.block_tile(*item_id), &gui.blocks)
                    {
                        // Block item: blit its block-atlas tile.
                        blit_image(
                            &mut pixels,
                            width,
                            height,
                            *dst,
                            blocks,
                            sx,
                            sy,
                            TILE_SIZE,
                            TILE_SIZE,
                        );
                    } else if let (Some((sx, sy)), Some(items)) =
                        (gui.items.tile_for_id(*item_id), gui.items.image())
                    {
                        // Item (tool/food/…): blit its item-atlas thumbnail.
                        blit_image(
                            &mut pixels,
                            width,
                            height,
                            *dst,
                            items,
                            sx,
                            sy,
                            TILE_SIZE,
                            TILE_SIZE,
                        );
                    } else {
                        // Unknown id: a deterministic tint swatch so it still reads.
                        fill_rect(
                            &mut pixels,
                            width,
                            height,
                            *dst,
                            item_swatch_color(*item_id),
                        );
                    }
                }
                UiCommand::GradientRect { rect, top, bottom } => {
                    gradient_rect(&mut pixels, width, height, scale_rect(*rect, s), *top, *bottom);
                }
            }
        }
        pixels
    }
}

/// The GUI pixel scale for a window of `width`×`height` (vanilla
/// `ScaledResolution` auto gui-scale with `guiScale=0`).
pub fn gui_pixel_scale(width: u32, height: u32) -> u32 {
    let mut scale = 1u32;
    while scale < 1000
        && width / (scale + 1) >= 320
        && height / (scale + 1) >= 240
    {
        scale += 1;
    }
    scale.max(1)
}

/// Divide a rect by the pixel scale, dividing the edges (not the size) so
/// adjacent rects stay seamless after rounding.
fn scale_rect(rect: UiRect, s: i32) -> UiRect {
    let x0 = rect.x / s;
    let y0 = rect.y / s;
    let x1 = (rect.x + rect.width) / s;
    let y1 = (rect.y + rect.height) / s;
    UiRect::new(x0, y0, x1 - x0, y1 - y0)
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

fn fill_rect(pixels: &mut [u8], width: u32, height: u32, rect: UiRect, color: UiColor) {
    let x0 = rect.x.max(0) as u32;
    let y0 = rect.y.max(0) as u32;
    let x1 = (rect.x + rect.width).clamp(0, width as i32) as u32;
    let y1 = (rect.y + rect.height).clamp(0, height as i32) as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            blend_pixel(pixels, width, x, y, color);
        }
    }
}

/// Vertical gradient (`top` at the top edge → `bottom` at the bottom), alpha-
/// composited (source-over) onto the buffer — vanilla `drawGradientRect`.
fn gradient_rect(pixels: &mut [u8], width: u32, height: u32, rect: UiRect, top: UiColor, bottom: UiColor) {
    let x0 = rect.x.max(0) as u32;
    let y0 = rect.y.max(0) as u32;
    let x1 = (rect.x + rect.width).clamp(0, width as i32) as u32;
    let y1 = (rect.y + rect.height).clamp(0, height as i32) as u32;
    let span = (rect.height - 1).max(1) as f32;
    for y in y0..y1 {
        let t = (y as i32 - rect.y) as f32 / span;
        let color = UiColor::rgba(
            lerp_u8(top.r, bottom.r, t),
            lerp_u8(top.g, bottom.g, t),
            lerp_u8(top.b, bottom.b, t),
            lerp_u8(top.a, bottom.a, t),
        );
        for x in x0..x1 {
            composite_pixel(pixels, width, x, y, color);
        }
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)).round() as u8
}

/// Source-over composite `src` onto the existing buffer pixel (so a translucent
/// tooltip border blends over the tooltip's own fill, like vanilla's GL blend).
fn composite_pixel(pixels: &mut [u8], width: u32, x: u32, y: u32, src: UiColor) {
    let index = ((y * width + x) * 4) as usize;
    let sa = src.a as f32 / 255.0;
    let da = pixels[index + 3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        return;
    }
    for c in 0..3 {
        let s = src_channel(src, c) as f32;
        let d = pixels[index + c] as f32;
        pixels[index + c] = ((s * sa + d * da * (1.0 - sa)) / out_a).round().min(255.0) as u8;
    }
    pixels[index + 3] = (out_a * 255.0).round().min(255.0) as u8;
}

fn src_channel(c: UiColor, i: usize) -> u8 {
    match i {
        0 => c.r,
        1 => c.g,
        _ => c.b,
    }
}

/// Tile `src` across `dst` at `tile_px` screen pixels per texture repeat,
/// multiplying `tint` into every texel (vanilla dirt background tint).
fn tile_image(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    dst: UiRect,
    src: &RgbaImage,
    tile_px: i32,
    tint: UiColor,
) {
    let (sw, sh) = src.dimensions();
    if sw == 0 || sh == 0 {
        return;
    }
    let y0 = dst.y.max(0);
    let y1 = (dst.y + dst.height).clamp(0, height as i32);
    let x0 = dst.x.max(0);
    let x1 = (dst.x + dst.width).clamp(0, width as i32);
    for ty in y0..y1 {
        let v = ((ty - dst.y) % tile_px) as u32 * sh / tile_px as u32;
        for tx in x0..x1 {
            let u = ((tx - dst.x) % tile_px) as u32 * sw / tile_px as u32;
            let texel = src.get_pixel(u.min(sw - 1), v.min(sh - 1)).0;
            let index = ((ty as u32 * width + tx as u32) * 4) as usize;
            pixels[index] = (texel[0] as u16 * tint.r as u16 / 255) as u8;
            pixels[index + 1] = (texel[1] as u16 * tint.g as u16 / 255) as u8;
            pixels[index + 2] = (texel[2] as u16 * tint.b as u16 / 255) as u8;
            pixels[index + 3] = tint.a;
        }
    }
}

/// Nearest-neighbour scale a source sub-rect into `dst`, alpha-compositing the
/// source over whatever is already in the UI buffer (so transparent texture
/// regions keep the content underneath).
#[allow(clippy::too_many_arguments)]
fn blit_image(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    dst: UiRect,
    src: &RgbaImage,
    sx: u32,
    sy: u32,
    sw: u32,
    sh: u32,
) {
    if dst.width <= 0 || dst.height <= 0 || sw == 0 || sh == 0 {
        return;
    }
    for dy in 0..dst.height {
        let ty = dst.y + dy;
        if ty < 0 || ty >= height as i32 {
            continue;
        }
        let v = sy + (dy as u32 * sh / dst.height as u32).min(sh - 1);
        for dx in 0..dst.width {
            let tx = dst.x + dx;
            if tx < 0 || tx >= width as i32 {
                continue;
            }
            let u = sx + (dx as u32 * sw / dst.width as u32).min(sw - 1);
            if u >= src.width() || v >= src.height() {
                continue;
            }
            let texel = src.get_pixel(u, v).0;
            let a = texel[3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            let index = ((ty as u32 * width + tx as u32) * 4) as usize;
            for c in 0..3 {
                let dst_c = pixels[index + c] as f32;
                pixels[index + c] = (texel[c] as f32 * a + dst_c * (1.0 - a)).round() as u8;
            }
            let dst_a = pixels[index + 3] as f32;
            pixels[index + 3] = (texel[3] as f32 + dst_a * (1.0 - a)).round().min(255.0) as u8;
        }
    }
}

fn blend_pixel(pixels: &mut [u8], width: u32, x: u32, y: u32, source: UiColor) {
    let index = ((y * width + x) * 4) as usize;
    pixels[index] = source.r;
    pixels[index + 1] = source.g;
    pixels[index + 2] = source.b;
    pixels[index + 3] = source.a;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(buf: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn gradient_rect_lerps_top_to_bottom_and_composites() {
        // A 4-px-tall purple gradient over a transparent buffer (pixel_scale 1).
        let mut frame = UiFrame::new();
        frame.overlay_gradient_rect(
            UiRect::new(0, 0, 2, 4),
            UiColor::rgba(80, 0, 255, 80),
            UiColor::rgba(40, 0, 127, 80),
        );
        let buf = UiFrame::rasterize(frame.overlay_commands(), 2, 4, 1, &GuiAtlas::default());
        // Over a transparent backdrop the source shows at its own color; the top
        // row is the start color, the bottom row the (darker/less-blue) end.
        let top = px(&buf, 2, 0, 0);
        let bottom = px(&buf, 2, 0, 3);
        assert_eq!(top, [80, 0, 255, 80]);
        assert_eq!(bottom, [40, 0, 127, 80]);
        assert!(bottom[2] < top[2], "blue channel decreases downward");
    }

    #[test]
    fn translucent_border_composites_over_the_fill() {
        // Fill (near-opaque dark purple) then a translucent border on top: the
        // border must blend with the fill, not the transparent backdrop.
        let mut frame = UiFrame::new();
        let bg = UiColor::rgba(16, 0, 16, 240);
        frame.overlay_gradient_rect(UiRect::new(0, 0, 1, 1), bg, bg);
        frame.overlay_gradient_rect(
            UiRect::new(0, 0, 1, 1),
            UiColor::rgba(80, 0, 255, 80),
            UiColor::rgba(80, 0, 255, 80),
        );
        let buf = UiFrame::rasterize(frame.overlay_commands(), 1, 1, 1, &GuiAtlas::default());
        let [r, _g, b, a] = px(&buf, 1, 0, 0);
        // Result sits between the dark fill and the bright purple border.
        assert!(a > 240, "compositing raises coverage above the fill alpha");
        assert!(b > 16 && b < 255, "blue blends between fill (16) and border (255)");
        assert!(r > 16, "red rises from the border contribution");
    }
}
