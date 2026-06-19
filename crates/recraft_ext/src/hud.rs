//! Host-agnostic HUD command buffer.
//!
//! A mod's `draw_hud` hook appends [`HudCmd`]s to a [`HudDraw`]; the host
//! (`recraft_app`) replays them into its `recraft_render::UiFrame`. Going through
//! a plain command list (rather than handing out `&mut UiFrame`) is what lets the
//! exact same HUD API cross the native ABI boundary later, and keeps `recraft_ext`
//! free of any render dependency.
//!
//! Colors are packed `0xRRGGBBAA`.

/// A registered texture handle (returned by `RegisterTexture` at load time).
/// The host owns the pixels and resolves the handle when replaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TexHandle(pub u32);

/// One HUD draw primitive. Coordinates are in GUI pixels (the host multiplies by
/// the active gui scale).
#[derive(Debug, Clone, PartialEq)]
pub enum HudCmd {
    Rect {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: u32,
    },
    Text {
        x: i32,
        y: i32,
        scale: i32,
        color: u32,
        text: String,
        shadow: bool,
    },
    /// Flat 2D item/block thumbnail.
    ItemIcon {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        item_id: i16,
    },
    /// 3D block cube rendered into the slot rect.
    BlockItem {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        block_id: u16,
        meta: u8,
    },
    /// A mod-supplied texture (registered at load time). `src` is an optional
    /// `(sx, sy, sw, sh)` source sub-rectangle (sprite sheets); `None` blits the
    /// whole image.
    Image {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        tex: TexHandle,
        src: Option<(u32, u32, u32, u32)>,
    },
    /// A straight line from `(x0,y0)` to `(x1,y1)`, `width` px thick.
    Line {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: u32,
        width: i32,
    },
    /// A vertical gradient rect (`top` color at the top edge → `bottom`).
    Gradient {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        top: u32,
        bottom: u32,
    },
}

/// The command buffer a mod appends to during `draw_hud`.
#[derive(Debug, Clone, Default)]
pub struct HudDraw {
    cmds: Vec<HudCmd>,
}

impl HudDraw {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn commands(&self) -> &[HudCmd] {
        &self.cmds
    }

    pub fn clear(&mut self) {
        self.cmds.clear();
    }

    pub fn push(&mut self, cmd: HudCmd) {
        self.cmds.push(cmd);
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        self.cmds.push(HudCmd::Rect { x, y, w, h, color });
    }

    pub fn text(&mut self, x: i32, y: i32, scale: i32, color: u32, text: impl Into<String>) {
        self.cmds.push(HudCmd::Text {
            x,
            y,
            scale,
            color,
            text: text.into(),
            shadow: true,
        });
    }

    pub fn text_plain(&mut self, x: i32, y: i32, scale: i32, color: u32, text: impl Into<String>) {
        self.cmds.push(HudCmd::Text {
            x,
            y,
            scale,
            color,
            text: text.into(),
            shadow: false,
        });
    }

    pub fn item_icon(&mut self, x: i32, y: i32, w: i32, h: i32, item_id: i16) {
        self.cmds.push(HudCmd::ItemIcon {
            x,
            y,
            w,
            h,
            item_id,
        });
    }

    pub fn block_item(&mut self, x: i32, y: i32, w: i32, h: i32, block_id: u16, meta: u8) {
        self.cmds.push(HudCmd::BlockItem {
            x,
            y,
            w,
            h,
            block_id,
            meta,
        });
    }

    pub fn image(&mut self, x: i32, y: i32, w: i32, h: i32, tex: TexHandle) {
        self.cmds.push(HudCmd::Image {
            x,
            y,
            w,
            h,
            tex,
            src: None,
        });
    }

    pub fn image_src(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        tex: TexHandle,
        src: (u32, u32, u32, u32),
    ) {
        self.cmds.push(HudCmd::Image {
            x,
            y,
            w,
            h,
            tex,
            src: Some(src),
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32, width: i32) {
        self.cmds.push(HudCmd::Line {
            x0,
            y0,
            x1,
            y1,
            color,
            width: width.max(1),
        });
    }

    pub fn gradient(&mut self, x: i32, y: i32, w: i32, h: i32, top: u32, bottom: u32) {
        self.cmds.push(HudCmd::Gradient {
            x,
            y,
            w,
            h,
            top,
            bottom,
        });
    }
}

/// Per-frame context handed to a `draw_hud` hook.
#[derive(Debug, Clone, Copy)]
pub struct HudCtx {
    /// Logical (gui-pixel) screen width.
    pub width: i32,
    /// Logical (gui-pixel) screen height.
    pub height: i32,
    /// Active gui scale factor.
    pub scale: i32,
    pub screen_open: bool,
}
