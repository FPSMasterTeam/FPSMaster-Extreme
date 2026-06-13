//! The chat input overlay (vanilla `GuiChat`). The world keeps ticking while
//! it is open; the HUD's chat panel renders the input box and the backlog —
//! this screen only owns the input state and key handling. Editing, cursor
//! movement and IME composition live in the shared [`TextInput`].

use recraft_render::UiFrame;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::chat::MAX_CHAT_INPUT;
use crate::text_input::TextInput;

use super::{DrawCtx, GuiAction, GuiScreen, ScreenCtx};

pub struct GuiChat {
    input: TextInput,
}

impl GuiChat {
    /// Open with pre-filled text ("" for T, "/" for the command key).
    pub fn new(prefill: impl Into<String>) -> Self {
        Self {
            input: TextInput::with_text(MAX_CHAT_INPUT, prefill),
        }
    }
}

impl GuiScreen for GuiChat {
    fn draw(&mut self, _ui: &mut UiFrame, _ctx: &DrawCtx) {
        // Drawn by GuiIngame via chat_input_mut().
    }

    fn key_pressed(&mut self, event: &KeyEvent, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state != ElementState::Pressed {
            return Vec::new();
        }
        match event.physical_key {
            PhysicalKey::Code(KeyCode::Escape) => return vec![GuiAction::CloseScreen],
            PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                let message = self.input.text().trim().to_owned();
                let mut actions = Vec::new();
                if !message.is_empty() {
                    actions.push(GuiAction::SendChat(message));
                }
                actions.push(GuiAction::CloseScreen);
                return actions;
            }
            // Up/Down browse the sent-message history (single-line: no caret use).
            PhysicalKey::Code(KeyCode::ArrowUp) => {
                if let Some(previous) = ctx.game.chat.recall_previous() {
                    self.input.set_text(previous);
                }
                return Vec::new();
            }
            PhysicalKey::Code(KeyCode::ArrowDown) => {
                if let Some(next) = ctx.game.chat.recall_next() {
                    self.input.set_text(next);
                }
                return Vec::new();
            }
            _ => {}
        }
        // Everything else (text, space, backspace, caret movement, paste) is
        // handled by the shared buffer.
        self.input
            .handle_key(event, ctx.modifiers, ctx.clipboard.as_deref_mut());
        Vec::new()
    }

    fn chat_input(&self) -> Option<&str> {
        Some(self.input.text())
    }

    fn chat_input_mut(&mut self) -> Option<&mut TextInput> {
        Some(&mut self.input)
    }

    fn focused_text_input(&mut self) -> Option<&mut TextInput> {
        Some(&mut self.input)
    }

    fn draws_over_hud(&self) -> bool {
        true
    }
}
