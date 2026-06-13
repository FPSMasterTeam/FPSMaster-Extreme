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
    /// gui/title/minecraft.png — the Minecraft title logo (256×256, two halves).
    Title,
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
        }
    }

    /// The block-atlas source pixel rect for a block item id's top-face texture,
    /// or None when the id isn't a known (non-air) block.
    fn block_tile(&self, item_id: i16) -> Option<(u32, u32)> {
        if item_id < 0 || item_id >= 256 {
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
    /// Vertical gradient from `top_color` to `bottom_color`.
    GradientRect {
        rect: UiRect,
        top_color: UiColor,
        bottom_color: UiColor,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiFrame {
    commands: Vec<UiCommand>,
}

impl UiFrame {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
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

    /// Vertical gradient from `top_color` to `bottom_color` across `rect`.
    pub fn gradient_rect(
        &mut self,
        rect: UiRect,
        top_color: UiColor,
        bottom_color: UiColor,
    ) {
        self.commands.push(UiCommand::GradientRect {
            rect,
            top_color,
            bottom_color,
        });
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

    /// Rasterize into a buffer downscaled by `pixel_scale` (the GUI pixel
    /// scale): command coordinates are divided by it, so the CPU rasterizes at
    /// GUI resolution and the GPU upscales nearest-neighbour — the vanilla
    /// chunky look at a fraction of the per-frame cost.
    pub fn rasterize(&self, width: u32, height: u32, pixel_scale: u32, gui: &GuiAtlas) -> Vec<u8> {
        let s = pixel_scale.max(1) as i32;
        let mut pixels = vec![0; width as usize * height as usize * 4];
        for command in &self.commands {
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
                UiCommand::GradientRect {
                    rect,
                    top_color,
                    bottom_color,
                } => {
                    let r = scale_rect(*rect, s);
                    gradient_rect(&mut pixels, width, height, r, *top_color, *bottom_color);
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

fn gradient_rect(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    rect: UiRect,
    top: UiColor,
    bottom: UiColor,
) {
    let x0 = rect.x.max(0) as u32;
    let y0 = rect.y.max(0) as u32;
    let x1 = (rect.x + rect.width).clamp(0, width as i32) as u32;
    let y1 = (rect.y + rect.height).clamp(0, height as i32) as u32;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let h = rect.height.max(1) as u32;
    let ry0 = rect.y.max(0) as u32;
    for y in y0..y1 {
        let row_offset = y - ry0;
        let a = lerp_u8_fast(top.a, bottom.a, row_offset, h);
        if a == 0 {
            continue;
        }
        let r = lerp_u8_fast(top.r, bottom.r, row_offset, h);
        let g = lerp_u8_fast(top.g, bottom.g, row_offset, h);
        let b = lerp_u8_fast(top.b, bottom.b, row_offset, h);
        let base = (y * width) as usize * 4;
        if a == 255 {
            for x in x0..x1 {
                let i = base + x as usize * 4;
                pixels[i] = r;
                pixels[i + 1] = g;
                pixels[i + 2] = b;
                pixels[i + 3] = 255;
            }
        } else {
            let a16 = a as u16;
            let inv = 255 - a16;
            let sr = r as u16 * a16;
            let sg = g as u16 * a16;
            let sb = b as u16 * a16;
            for x in x0..x1 {
                let i = base + x as usize * 4;
                pixels[i] = ((sr + pixels[i] as u16 * inv) / 255) as u8;
                pixels[i + 1] = ((sg + pixels[i + 1] as u16 * inv) / 255) as u8;
                pixels[i + 2] = ((sb + pixels[i + 2] as u16 * inv) / 255) as u8;
                pixels[i + 3] =
                    (a16 + pixels[i + 3] as u16 * inv / 255).min(255) as u8;
            }
        }
    }
}

fn lerp_u8_fast(a: u8, b: u8, num: u32, den: u32) -> u8 {
    let a = a as i32;
    let b = b as i32;
    (a + (b - a) * num as i32 / den.max(1) as i32).clamp(0, 255) as u8
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

