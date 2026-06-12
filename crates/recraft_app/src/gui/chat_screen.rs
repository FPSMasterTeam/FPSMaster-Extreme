//! The chat input overlay (vanilla `GuiChat`). The world keeps ticking while
//! it is open; the HUD's chat panel renders the input box and the backlog —
//! this screen only owns the input state and key handling.

use recraft_render::UiFrame;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, PhysicalKey};

use crate::chat::MAX_CHAT_INPUT;

use super::{DrawCtx, GuiAction, GuiScreen, ScreenCtx};

pub struct GuiChat {
    input: String,
}

impl GuiChat {
    /// Open with pre-filled text ("" for T, "/" for the command key).
    pub fn new(prefill: impl Into<String>) -> Self {
        Self {
            input: prefill.into(),
        }
    }
}

impl GuiScreen for GuiChat {
    fn draw(&mut self, _ui: &mut UiFrame, _ctx: &DrawCtx) {
        // Drawn by GuiIngame via chat_input().
    }

    fn key_pressed(&mut self, event: &KeyEvent, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state != ElementState::Pressed {
            return Vec::new();
        }
        match event.physical_key {
            PhysicalKey::Code(KeyCode::Escape) => vec![GuiAction::CloseScreen],
            PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                let message = self.input.trim().to_owned();
                let mut actions = Vec::new();
                if !message.is_empty() {
                    actions.push(GuiAction::SendChat(message));
                }
                actions.push(GuiAction::CloseScreen);
                actions
            }
            PhysicalKey::Code(KeyCode::Backspace) => {
                self.input.pop();
                Vec::new()
            }
            PhysicalKey::Code(KeyCode::ArrowUp) => {
                if let Some(previous) = ctx.game.chat.recall_previous() {
                    self.input = previous;
                }
                Vec::new()
            }
            PhysicalKey::Code(KeyCode::ArrowDown) => {
                if let Some(next) = ctx.game.chat.recall_next() {
                    self.input = next;
                }
                Vec::new()
            }
            // Space arrives as Key::Named(Space), not as a Character.
            PhysicalKey::Code(KeyCode::Space) => {
                if self.input.chars().count() < MAX_CHAT_INPUT {
                    self.input.push(' ');
                }
                Vec::new()
            }
            _ => {
                if let Key::Character(s) = &event.logical_key {
                    for c in s.chars() {
                        if !c.is_control() && self.input.chars().count() < MAX_CHAT_INPUT {
                            self.input.push(c);
                        }
                    }
                }
                Vec::new()
            }
        }
    }

    fn chat_input(&self) -> Option<&str> {
        Some(&self.input)
    }

    fn draws_over_hud(&self) -> bool {
        true
    }
}
