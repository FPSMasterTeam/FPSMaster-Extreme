//! Vanilla-styled widgets: buttons and text fields drawn from
//! `gui/widgets.png` with the hover / disabled texture states and the
//! vanilla label colors.

use recraft_render::{text_width, GuiTexture, UiColor, UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, PhysicalKey};

/// Vanilla button label colors.
const LABEL_ENABLED: UiColor = UiColor::rgba(224, 224, 224, 255);
const LABEL_HOVERED: UiColor = UiColor::rgba(255, 255, 160, 255);
const LABEL_DISABLED: UiColor = UiColor::rgba(160, 160, 160, 255);

/// widgets.png button strip rows (200x20 each).
const BUTTON_TEX_DISABLED: u32 = 46;
const BUTTON_TEX_IDLE: u32 = 66;
const BUTTON_TEX_HOVER: u32 = 86;
/// Button height in GUI px (vanilla buttons are always 20 tall).
pub const BUTTON_HEIGHT: i32 = 20;

/// A vanilla-textured push button (`GuiButton`).
pub struct GuiButton {
    pub rect: UiRect,
    pub label: String,
    pub enabled: bool,
}

impl GuiButton {
    /// Build directly from a screen-pixel rect (height forced to 20 GUI px).
    pub fn at_px(x: i32, y: i32, width: i32, scale: i32, label: impl Into<String>) -> Self {
        Self {
            rect: UiRect::new(x, y, width, BUTTON_HEIGHT * scale),
            label: label.into(),
            enabled: true,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.enabled = !disabled;
        self
    }

    pub fn hovered(&self, mouse: (f64, f64)) -> bool {
        self.rect.contains(mouse.0, mouse.1)
    }

    /// Whether a click at (x, y) presses this button.
    pub fn clicked(&self, x: f64, y: f64) -> bool {
        self.enabled && self.rect.contains(x, y)
    }

    /// Draw the button: the widgets.png state texture (split in half so any
    /// width keeps the beveled ends) and the shadowed label, which dips one
    /// pixel while held — the hover/press feedback.
    pub fn draw(&self, ui: &mut UiFrame, scale: i32, mouse: (f64, f64), mouse_down: bool) {
        let hovered = self.enabled && self.hovered(mouse);
        let pressed = hovered && mouse_down;
        let tex_y = if !self.enabled {
            BUTTON_TEX_DISABLED
        } else if hovered {
            BUTTON_TEX_HOVER
        } else {
            BUTTON_TEX_IDLE
        };

        // Flat fallback behind the texture so buttons still read without assets.
        ui.rect(self.rect, UiColor::rgba(70, 70, 70, 220));
        let width_gui = (self.rect.width / scale).clamp(2, 200);
        let left_gui = width_gui / 2;
        let right_gui = width_gui - left_gui;
        ui.image(
            UiRect::new(self.rect.x, self.rect.y, left_gui * scale, self.rect.height),
            GuiTexture::Widgets,
            0,
            tex_y,
            left_gui as u32,
            BUTTON_HEIGHT as u32,
        );
        ui.image(
            UiRect::new(
                self.rect.x + left_gui * scale,
                self.rect.y,
                self.rect.width - left_gui * scale,
                self.rect.height,
            ),
            GuiTexture::Widgets,
            (200 - right_gui) as u32,
            tex_y,
            right_gui as u32,
            BUTTON_HEIGHT as u32,
        );

        let color = if !self.enabled {
            LABEL_DISABLED
        } else if hovered {
            LABEL_HOVERED
        } else {
            LABEL_ENABLED
        };
        let label = super::fit_text(&self.label, self.rect.width - 8 * scale, scale);
        let text_x = self.rect.x + (self.rect.width - text_width(&label, scale)) / 2;
        let text_y = self.rect.y + (self.rect.height - 8 * scale) / 2 + i32::from(pressed) * scale;
        ui.text_shadowed(text_x, text_y, scale, color, label);
    }
}

/// A vanilla-styled single-line text field (`GuiTextField`).
pub struct GuiTextField {
    pub rect: UiRect,
    pub text: String,
    pub focused: bool,
    pub max_len: usize,
    /// Show only a character count instead of the content (token entry).
    pub masked: bool,
}

impl GuiTextField {
    pub fn new(rect: UiRect, max_len: usize) -> Self {
        Self {
            rect,
            text: String::new(),
            focused: true,
            max_len,
            masked: false,
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    /// Focus follows clicks (click inside focuses, outside blurs).
    pub fn mouse_clicked(&mut self, x: f64, y: f64) {
        self.focused = self.rect.contains(x, y);
    }

    /// Consume a key event when focused: backspace, space and characters.
    /// Returns true when the event edited the field.
    pub fn key_pressed(&mut self, event: &KeyEvent) -> bool {
        if !self.focused || event.state != ElementState::Pressed {
            return false;
        }
        match event.physical_key {
            PhysicalKey::Code(KeyCode::Backspace) => {
                self.text.pop();
                true
            }
            // Space arrives as Key::Named(Space), not a Character.
            PhysicalKey::Code(KeyCode::Space) => {
                if self.text.chars().count() < self.max_len {
                    self.text.push(' ');
                }
                true
            }
            _ => {
                if let Key::Character(s) = &event.logical_key {
                    let mut edited = false;
                    for c in s.chars() {
                        if !c.is_control() && self.text.chars().count() < self.max_len {
                            self.text.push(c);
                            edited = true;
                        }
                    }
                    edited
                } else {
                    false
                }
            }
        }
    }

    /// Vanilla text-field look: black fill, gray border, light gray text with
    /// a trailing caret while focused; overflowing text shows its tail.
    pub fn draw(&self, ui: &mut UiFrame, scale: i32) {
        let border = UiColor::rgba(160, 160, 160, 255);
        ui.rect(
            UiRect::new(
                self.rect.x - scale,
                self.rect.y - scale,
                self.rect.width + 2 * scale,
                self.rect.height + 2 * scale,
            ),
            border,
        );
        ui.rect(self.rect, UiColor::rgba(0, 0, 0, 255));

        let display = if self.masked && !self.text.is_empty() {
            format!("{} characters", self.text.chars().count())
        } else {
            self.text.clone()
        };
        let display = if self.focused {
            format!("{display}_")
        } else {
            display
        };
        let avail = self.rect.width - 8 * scale;
        let visible = trim_to_tail(&display, avail, scale);
        ui.text_shadowed(
            self.rect.x + 4 * scale,
            self.rect.y + (self.rect.height - 8 * scale) / 2,
            scale,
            UiColor::rgba(224, 224, 224, 255),
            visible,
        );
    }
}

/// Drop characters from the front until the text fits `max_width` px — keeps
/// the tail of an overflowing input visible.
pub(crate) fn trim_to_tail(text: &str, max_width: i32, scale: i32) -> String {
    if text_width(text, scale) <= max_width {
        return text.to_owned();
    }
    for (index, _) in text.char_indices() {
        if text_width(&text[index..], scale) <= max_width {
            return text[index..].to_owned();
        }
    }
    String::new()
}
