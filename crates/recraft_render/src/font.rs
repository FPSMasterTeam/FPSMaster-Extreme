//! Vanilla 1.8 font rendering, ported from the client's `FontRenderer`.
//!
//! Glyphs come from `textures/font/ascii.png` (a 16x16 grid; per-character
//! advances are computed by scanning each cell for its rightmost opaque
//! column, exactly like `readFontTexture`). Characters outside the ascii
//! sheet's 256-slot charset fall back to the `unicode_page_XX.png` sheets
//! (16px glyphs drawn at half size) with widths from `font/glyph_sizes.bin` —
//! this is what renders CJK chat. Legacy `§` style codes are applied during
//! drawing: the 16 colors, bold (re-draw offset by 1px), italic (1px shear),
//! strikethrough, underline; a color code resets formatting, `§r` resets all.
//! Shadowed text re-draws at +1,+1 with every color quartered, like vanilla.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use image::{imageops::FilterType, DynamicImage, RgbaImage};

use crate::texture;

/// The character each `ascii.png` slot represents (vanilla `FontRenderer`
/// charset: a few accented letters, ASCII, then the CP437 high half).
const ASCII_CHARS: &str = "\
ÀÁÂÈÊËÍÓÔÕÚßãõğİıŒœŞşŴŵžȇ\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0} \
!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~\u{0}\
ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀\
αβΓπΣσμτΦΘΩδ∞∅∈∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{0}";

/// The vanilla 16-color legacy palette (`§0`..`§f`).
pub fn legacy_color(code: char) -> Option<[u8; 3]> {
    Some(match code.to_ascii_lowercase() {
        '0' => [0, 0, 0],
        '1' => [0, 0, 170],
        '2' => [0, 170, 0],
        '3' => [0, 170, 170],
        '4' => [170, 0, 0],
        '5' => [170, 0, 170],
        '6' => [255, 170, 0],
        '7' => [170, 170, 170],
        '8' => [85, 85, 85],
        '9' => [85, 85, 255],
        'a' => [85, 255, 85],
        'b' => [85, 255, 255],
        'c' => [255, 85, 85],
        'd' => [255, 85, 255],
        'e' => [255, 255, 85],
        'f' => [255, 255, 255],
        _ => return None,
    })
}

pub struct Font {
    /// ascii.png normalized to 128x128 (8px cells).
    ascii: RgbaImage,
    /// Per-slot advance in font px (vanilla charWidth), space forced to 4.
    advance: [u8; 256],
    /// Character -> ascii.png slot.
    index_of: HashMap<char, u8>,
    /// glyph_sizes.bin: per-codepoint start/end column nibbles, if available.
    glyph_sizes: Option<Vec<u8>>,
    /// Lazily loaded unicode_page_XX sheets (None = load failed/missing).
    pages: Mutex<HashMap<u8, Option<Arc<RgbaImage>>>>,
}

static FONT: OnceLock<Font> = OnceLock::new();

pub fn font() -> &'static Font {
    FONT.get_or_init(Font::load_default)
}

/// Advance of a single character in screen px at `scale` (no bold).
pub fn char_width(c: char, scale: i32) -> i32 {
    font().advance_px(c, scale.max(1))
}

/// Per-character §-aware drawing state.
#[derive(Clone, Copy)]
struct Style {
    color: [u8; 3],
    bold: bool,
    strikethrough: bool,
    underline: bool,
    italic: bool,
}

impl Font {
    fn load_default() -> Self {
        let ascii = texture::load_asset_image("assets/minecraft/textures/font/ascii.png")
            .map(|image| {
                if image.dimensions() == (128, 128) {
                    image
                } else {
                    DynamicImage::ImageRgba8(image)
                        .resize_exact(128, 128, FilterType::Nearest)
                        .to_rgba8()
                }
            })
            .unwrap_or_else(|| {
                log::warn!("font ascii.png not found; using built-in fallback glyphs");
                fallback_ascii_image()
            });

        let mut advance = [0u8; 256];
        for (slot, width) in advance.iter_mut().enumerate() {
            *width = scan_advance(&ascii, slot as u32);
        }
        advance[32] = 4; // vanilla forces the space advance

        let mut index_of = HashMap::with_capacity(256);
        for (slot, c) in ASCII_CHARS.chars().enumerate() {
            if c != '\u{0}' {
                index_of.insert(c, slot as u8);
            }
        }

        let glyph_sizes = texture::load_asset_bytes("assets/minecraft/font/glyph_sizes.bin")
            .filter(|bytes| bytes.len() == 65536);
        if glyph_sizes.is_none() {
            log::info!("font glyph_sizes.bin not found; non-ASCII text will render as '?'");
        }

        Self {
            ascii,
            advance,
            index_of,
            glyph_sizes,
            pages: Mutex::new(HashMap::new()),
        }
    }

    /// Width in screen px of §-coded text at `scale`. Bold adds one font px
    /// per character; a color code or `§r` resets bold (matching how the
    /// renderer applies codes, so measure and draw always agree).
    pub fn text_width(&self, text: &str, scale: i32) -> i32 {
        let scale = scale.max(1);
        let mut width = 0;
        let mut bold = false;
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '§' {
                match chars.next() {
                    Some(code) if code == 'l' || code == 'L' => bold = true,
                    Some(code) if legacy_color(code).is_some() || code == 'r' || code == 'R' => {
                        bold = false
                    }
                    _ => {}
                }
                continue;
            }
            let advance = self.advance_px(c, scale);
            width += advance;
            if bold && advance > 0 {
                width += scale;
            }
        }
        width
    }

    /// Rasterize §-coded text into an RGBA buffer. `base` is the default
    /// color; its alpha applies to everything (including §-recolored runs).
    /// `darken` quarters every color — the vanilla shadow pass.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        pixels: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: i32,
        y: i32,
        scale: i32,
        base: [u8; 4],
        darken: bool,
        text: &str,
    ) {
        let scale = scale.max(1);
        let base_rgb = if darken {
            [base[0] / 4, base[1] / 4, base[2] / 4]
        } else {
            [base[0], base[1], base[2]]
        };
        let alpha = base[3];
        let reset = Style {
            color: base_rgb,
            bold: false,
            strikethrough: false,
            underline: false,
            italic: false,
        };
        let mut style = reset;
        let mut cursor = x;
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '§' {
                let Some(code) = chars.next() else { break };
                if let Some(rgb) = legacy_color(code) {
                    // A color code clears the formatting flags (vanilla
                    // renderStringAtPos semantics).
                    style = reset;
                    style.color = if darken {
                        [rgb[0] / 4, rgb[1] / 4, rgb[2] / 4]
                    } else {
                        rgb
                    };
                } else {
                    match code.to_ascii_lowercase() {
                        'l' => style.bold = true,
                        'm' => style.strikethrough = true,
                        'n' => style.underline = true,
                        'o' => style.italic = true,
                        'r' => style = reset,
                        _ => {} // 'k' obfuscated: rendered unscrambled
                    }
                }
                continue;
            }

            let color = [style.color[0], style.color[1], style.color[2], alpha];
            let mut advance = self.advance_px(c, scale);
            self.draw_glyph(
                pixels,
                buf_width,
                buf_height,
                cursor,
                y,
                scale,
                color,
                c,
                style.italic,
            );
            if style.bold && advance > 0 {
                self.draw_glyph(
                    pixels,
                    buf_width,
                    buf_height,
                    cursor + scale,
                    y,
                    scale,
                    color,
                    c,
                    style.italic,
                );
                advance += scale;
            }
            // Vanilla line positions: strikethrough at row 3, underline at row
            // 8 extending one px to the left.
            if style.strikethrough {
                fill_rect(
                    pixels,
                    buf_width,
                    buf_height,
                    cursor,
                    y + 3 * scale,
                    advance,
                    scale,
                    color,
                );
            }
            if style.underline {
                fill_rect(
                    pixels,
                    buf_width,
                    buf_height,
                    cursor - scale,
                    y + 8 * scale,
                    advance + scale,
                    scale,
                    color,
                );
            }
            cursor += advance;
        }
    }

    /// Advance of one character in screen px at `scale` (without bold).
    fn advance_px(&self, c: char, scale: i32) -> i32 {
        if let Some(&slot) = self.index_of.get(&c) {
            return self.advance[slot as usize] as i32 * scale;
        }
        // Astral-plane codepoints (emoji etc.) have no glyph pages: hidden,
        // zero width — never shown as '?'.
        if c as u32 > 0xFFFF {
            return 0;
        }
        if self.glyph_sizes.is_some() {
            return match self.unicode_span(c) {
                // Vanilla unicode advance: (end + 1 - start) / 2 + 1 (in font
                // px, can be fractional); rounded once so measure == draw.
                Some((start, end)) => (((end + 1 - start) + 2) * scale + 1) / 2,
                // Glyph data says width 0: vanilla hides the character.
                None => 0,
            };
        }
        // No glyph data at all (assets missing): show the fallback glyph.
        self.advance[self.fallback_slot()] as i32 * scale
    }

    /// The slot used for characters we cannot render ('?').
    fn fallback_slot(&self) -> usize {
        self.index_of.get(&'?').map_or(0, |&slot| slot as usize)
    }

    /// glyph_sizes.bin start/end columns for a codepoint, when renderable
    /// from the unicode pages.
    fn unicode_span(&self, c: char) -> Option<(i32, i32)> {
        let sizes = self.glyph_sizes.as_ref()?;
        let cp = c as u32;
        if cp > 0xFFFF {
            return None;
        }
        let byte = sizes[cp as usize];
        if byte == 0 {
            return None;
        }
        Some(((byte >> 4) as i32, (byte & 15) as i32))
    }

    fn unicode_page(&self, page: u8) -> Option<Arc<RgbaImage>> {
        let mut pages = self.pages.lock().expect("font page cache poisoned");
        pages
            .entry(page)
            .or_insert_with(|| {
                let asset = format!("assets/minecraft/textures/font/unicode_page_{page:02x}.png");
                texture::load_asset_image(&asset).map(|image| {
                    let image = if image.dimensions() == (256, 256) {
                        image
                    } else {
                        DynamicImage::ImageRgba8(image)
                            .resize_exact(256, 256, FilterType::Nearest)
                            .to_rgba8()
                    };
                    Arc::new(image)
                })
            })
            .clone()
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_glyph(
        &self,
        pixels: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: i32,
        y: i32,
        scale: i32,
        color: [u8; 4],
        c: char,
        italic: bool,
    ) {
        if let Some(&slot) = self.index_of.get(&c) {
            let advance = self.advance[slot as usize] as i32;
            if advance > 1 {
                let sx = (slot as u32 % 16) * 8;
                let sy = (slot as u32 / 16) * 8;
                blit_glyph(
                    pixels,
                    buf_width,
                    buf_height,
                    &self.ascii,
                    sx,
                    sy,
                    (advance - 1) as u32,
                    8,
                    x,
                    y,
                    (advance - 1) * scale,
                    8 * scale,
                    scale,
                    color,
                    italic,
                );
            }
            return;
        }
        // Hidden characters (mirrors advance_px): astral-plane codepoints and
        // anything the glyph data marks as zero-width draw nothing.
        if c as u32 > 0xFFFF {
            return;
        }
        if self.glyph_sizes.is_some() {
            if let Some((start, end)) = self.unicode_span(c) {
                let cp = c as u32;
                if let Some(page) = self.unicode_page((cp >> 8) as u8) {
                    let sx = (cp % 16) * 16 + start as u32;
                    let sy = (cp / 16 % 16) * 16;
                    let src_w = (end + 1 - start) as u32;
                    let dest_w = (src_w as i32 * scale + 1) / 2;
                    blit_glyph(
                        pixels,
                        buf_width,
                        buf_height,
                        &page,
                        sx,
                        sy,
                        src_w,
                        16,
                        x,
                        y,
                        dest_w,
                        8 * scale,
                        scale,
                        color,
                        italic,
                    );
                }
            }
            return;
        }
        // No glyph data at all: draw the fallback glyph.
        if c != '?' {
            self.draw_glyph(
                pixels, buf_width, buf_height, x, y, scale, color, '?', italic,
            );
        }
    }
}

/// Vanilla `readFontTexture` width scan for one 8x8 cell of a 128x128
/// ascii.png: rightmost column containing any non-transparent pixel + 2.
fn scan_advance(ascii: &RgbaImage, slot: u32) -> u8 {
    let x0 = slot % 16 * 8;
    let y0 = slot / 16 * 8;
    for col in (0..8u32).rev() {
        for row in 0..8u32 {
            if ascii.get_pixel(x0 + col, y0 + row)[3] != 0 {
                return (col + 2) as u8;
            }
        }
    }
    1 // empty cell
}

/// Built-in stand-in for a missing ascii.png: the charset's ASCII range
/// rendered from the embedded font8x8 glyphs (other slots stay empty).
fn fallback_ascii_image() -> RgbaImage {
    use font8x8::UnicodeFonts;
    let mut image = RgbaImage::new(128, 128);
    for (slot, c) in ASCII_CHARS.chars().enumerate() {
        let Some(glyph) = font8x8::BASIC_FONTS.get(c) else {
            continue;
        };
        let x0 = (slot as u32 % 16) * 8;
        let y0 = (slot as u32 / 16) * 8;
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8u32 {
                if bits & (1 << col) != 0 {
                    image.put_pixel(x0 + col, y0 + row as u32, image::Rgba([255, 255, 255, 255]));
                }
            }
        }
    }
    image
}

/// Nearest-neighbour blit of a glyph source rect, tinted by `color`, with the
/// vanilla italic shear (top edge +1 font px, bottom edge -1) when requested.
#[allow(clippy::too_many_arguments)]
fn blit_glyph(
    pixels: &mut [u8],
    buf_width: u32,
    buf_height: u32,
    src: &RgbaImage,
    sx: u32,
    sy: u32,
    src_w: u32,
    src_h: u32,
    x: i32,
    y: i32,
    dest_w: i32,
    dest_h: i32,
    scale: i32,
    color: [u8; 4],
    italic: bool,
) {
    if dest_w <= 0 || dest_h <= 0 || src_w == 0 || src_h == 0 || color[3] == 0 {
        return;
    }
    for dy in 0..dest_h {
        let ty = y + dy;
        if ty < 0 || ty >= buf_height as i32 {
            continue;
        }
        let shear = if italic {
            // Linear from +scale at the top edge to -scale at the bottom.
            ((1.0 - 2.0 * (dy as f32 + 0.5) / dest_h as f32) * scale as f32).round() as i32
        } else {
            0
        };
        let v = sy + (dy as u32 * src_h / dest_h as u32).min(src_h - 1);
        for dx in 0..dest_w {
            let tx = x + dx + shear;
            if tx < 0 || tx >= buf_width as i32 {
                continue;
            }
            let u = sx + (dx as u32 * src_w / dest_w as u32).min(src_w - 1);
            if u >= src.width() || v >= src.height() {
                continue;
            }
            let texel = src.get_pixel(u, v).0;
            let rgba = [
                (texel[0] as u16 * color[0] as u16 / 255) as u8,
                (texel[1] as u16 * color[1] as u16 / 255) as u8,
                (texel[2] as u16 * color[2] as u16 / 255) as u8,
                (texel[3] as u16 * color[3] as u16 / 255) as u8,
            ];
            composite_pixel(pixels, buf_width, tx as u32, ty as u32, rgba);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect(
    pixels: &mut [u8],
    buf_width: u32,
    buf_height: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: [u8; 4],
) {
    for ty in y.max(0)..(y + h).min(buf_height as i32) {
        for tx in x.max(0)..(x + w).min(buf_width as i32) {
            composite_pixel(pixels, buf_width, tx as u32, ty as u32, color);
        }
    }
}

/// Source-over composite of one RGBA pixel into the UI buffer.
fn composite_pixel(pixels: &mut [u8], buf_width: u32, x: u32, y: u32, rgba: [u8; 4]) {
    if rgba[3] == 0 {
        return;
    }
    let index = ((y * buf_width + x) * 4) as usize;
    let a = rgba[3] as f32 / 255.0;
    for c in 0..3 {
        let dst = pixels[index + c] as f32;
        pixels[index + c] = (rgba[c] as f32 * a + dst * (1.0 - a)).round() as u8;
    }
    let dst_a = pixels[index + 3] as f32;
    pixels[index + 3] = (rgba[3] as f32 + dst_a * (1.0 - a)).round().min(255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charset_has_exactly_256_slots() {
        assert_eq!(ASCII_CHARS.chars().count(), 256);
        // Spot-check well-known slots.
        let chars: Vec<char> = ASCII_CHARS.chars().collect();
        assert_eq!(chars[32], ' ');
        assert_eq!(chars[33], '!');
        assert_eq!(chars[65], 'A');
        assert_eq!(chars[126], '~');
    }

    #[test]
    fn widths_are_additive_and_skip_codes() {
        let f = font();
        let a = f.text_width("a", 2);
        let b = f.text_width("b", 2);
        assert!(a > 0 && b > 0);
        assert_eq!(f.text_width("ab", 2), a + b);
        assert_eq!(f.text_width("§cab", 2), a + b);
        assert_eq!(f.text_width("§", 2), 0); // bare trailing code is safe
    }

    #[test]
    fn space_advance_is_four_font_px() {
        assert_eq!(char_width(' ', 1), 4);
        assert_eq!(char_width(' ', 3), 12);
    }

    #[test]
    fn bold_adds_one_font_px_per_character() {
        let f = font();
        let plain = f.text_width("ab", 3);
        assert_eq!(f.text_width("§lab", 3), plain + 2 * 3);
        // Color codes reset bold.
        assert_eq!(f.text_width("§la§cb", 3), plain + 3);
    }

    #[test]
    fn astral_plane_characters_are_hidden() {
        let f = font();
        // Emoji (beyond U+FFFF) take no space and draw nothing, with or
        // without font assets present.
        assert_eq!(char_width('😀', 2), 0);
        assert_eq!(f.text_width("a😀b", 2), f.text_width("ab", 2));
        let (w, h) = (32u32, 16u32);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        f.draw(
            &mut pixels,
            w,
            h,
            1,
            1,
            1,
            [255, 255, 255, 255],
            false,
            "😀",
        );
        assert!(pixels.iter().all(|&b| b == 0), "emoji must not be drawn");
    }

    #[test]
    fn draw_tints_and_composites_into_buffer() {
        let f = font();
        let (w, h) = (64u32, 16u32);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        f.draw(&mut pixels, w, h, 1, 1, 1, [255, 0, 0, 255], false, "A");
        // Some pixel should now be red-ish and opaque.
        let mut hit = false;
        for px in pixels.chunks(4) {
            if px[3] > 0 {
                assert!(px[0] >= px[1] && px[0] >= px[2]);
                hit = true;
            }
        }
        assert!(hit, "drawing 'A' should write pixels");
    }
}
