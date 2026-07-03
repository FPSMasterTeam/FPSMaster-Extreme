//! The chat input overlay (vanilla `GuiChat`). The world keeps ticking while
//! it is open; the HUD's chat panel renders the input box and the backlog —
//! this screen only owns the input state and key handling. Editing, cursor
//! movement and IME composition live in the shared [`TextInput`].

use fpsmaster_protocol::v1_8_9::packets::ServerboundPacket;
use fpsmaster_render::{UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::chat::{ClickAction, ClickEvent, MAX_CHAT_INPUT};
use crate::text_input::TextInput;

use super::{ingame, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

pub struct GuiChat {
    input: TextInput,
    /// Clickable backlog regions cached from the last `draw` (screen px), so a
    /// mouse click can be mapped to a chat component's action.
    click_regions: Vec<(UiRect, ClickEvent)>,
}

impl GuiChat {
    /// Open with pre-filled text ("" for T, "/" for the command key).
    pub fn new(prefill: impl Into<String>) -> Self {
        Self {
            input: TextInput::with_text(MAX_CHAT_INPUT, prefill),
            click_regions: Vec::new(),
        }
    }
}

impl GuiScreen for GuiChat {
    fn draw(&mut self, _ui: &mut UiFrame, ctx: &DrawCtx) {
        // The HUD draws the chat panel (via chat_input_mut()); here we only
        // rebuild the clickable backlog layout so clicks can be hit-tested.
        self.click_regions = match ctx.hud {
            Some(hud) => ingame::chat_click_regions(ctx.width, ctx.height, hud.chat),
            None => Vec::new(),
        };
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
            // Tab requests completions on the first press, then cycles through
            // the stored candidates on each repeat (vanilla GuiChat behaviour).
            PhysicalKey::Code(KeyCode::Tab) => {
                if ctx.game.chat.has_completions() {
                    if let Some(completion) = ctx.game.chat.next_completion() {
                        let completed = crate::chat::complete_word(self.input.text(), completion);
                        self.input.set_text(completed);
                    }
                    return Vec::new();
                }
                return vec![GuiAction::SendPacket(ServerboundPacket::TabComplete {
                    text: self.input.text().to_owned(),
                    has_position: false,
                })];
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
        // handled by the shared buffer. Any edit invalidates a pending
        // completion cycle so a stale candidate can't be applied.
        ctx.game.chat.clear_completions();
        self.input
            .handle_key(event, ctx.modifiers, ctx.clipboard.as_deref_mut());
        Vec::new()
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        let (mx, my) = (x as i32, y as i32);
        let Some((_, event)) = self
            .click_regions
            .iter()
            .find(|(rect, _)| contains(rect, mx, my))
        else {
            return Vec::new();
        };
        match event.action {
            ClickAction::RunCommand => vec![GuiAction::SendChat(event.value.clone())],
            ClickAction::SuggestCommand => {
                self.input.set_text(event.value.clone());
                Vec::new()
            }
            ClickAction::OpenUrl => vec![GuiAction::OpenUrl(event.value.clone())],
            ClickAction::CopyToClipboard => {
                if let Some(clipboard) = ctx.clipboard.as_deref_mut() {
                    let _ = clipboard.set_text(event.value.clone());
                }
                Vec::new()
            }
        }
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

    // Chat keeps the world bright (no scrim), vanilla-style.
    fn dims_world(&self) -> bool {
        false
    }
}

fn contains(rect: &UiRect, x: i32, y: i32) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}
