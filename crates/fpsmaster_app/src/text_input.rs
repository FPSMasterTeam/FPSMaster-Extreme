//! A reusable single-line text-editing buffer shared by every place the user
//! types: the chat overlay and the `GuiTextField` widget (server name/address,
//! direct-connect, token entry). Centralizing it means cursor movement,
//! clipboard paste and — crucially — IME composition (Chinese/Japanese/Korean
//! input) behave identically everywhere instead of each screen re-implementing
//! append/backspace.
//!
//! IME flow (winit `Ime` events): while an input method is composing, the
//! in-progress text arrives as [`Ime::Preedit`] and is shown inline at the
//! caret without being part of [`text`](TextInput::text); the final characters
//! arrive as [`Ime::Commit`] and are inserted. On macOS plain Latin keystrokes
//! still come through `KeyEvent` (handled by [`handle_key`](TextInput::handle_key)),
//! and no `KeyEvent` is delivered while a composition is active, so the two
//! paths never double-insert.

use winit::event::{ElementState, Ime, KeyEvent};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// A single-line editable text buffer with a caret and IME composition state.
pub struct TextInput {
    /// Committed text (the composition preview is *not* part of this).
    text: String,
    /// Caret position as a byte index into `text` (always a char boundary).
    cursor: usize,
    /// Maximum length in characters (Unicode scalar values).
    max_chars: usize,
    /// Active IME composition string, shown inline at the caret but not yet
    /// committed. Empty when no composition is in progress.
    preedit: String,
    /// Last caret rectangle in physical screen px `(x, y, w, h)`, set by the
    /// drawing code so the host can position the IME candidate window.
    caret_area: Option<(i32, i32, i32, i32)>,
}

impl TextInput {
    pub fn new(max_chars: usize) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            max_chars,
            preedit: String::new(),
            caret_area: None,
        }
    }

    /// Build with initial content (caret placed at the end).
    pub fn with_text(max_chars: usize, text: impl Into<String>) -> Self {
        let mut input = Self::new(max_chars);
        input.set_text(text);
        input
    }

    /// Replace the whole buffer (caret to the end, composition cleared).
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        // Clamp to the char limit, keeping a valid char boundary.
        if self.text.chars().count() > self.max_chars {
            let end = self
                .text
                .char_indices()
                .nth(self.max_chars)
                .map_or(self.text.len(), |(i, _)| i);
            self.text.truncate(end);
        }
        self.cursor = self.text.len();
        self.preedit.clear();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether an IME composition is currently in progress.
    pub fn is_composing(&self) -> bool {
        !self.preedit.is_empty()
    }

    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The three display segments around the caret: text before the caret, the
    /// composition preview, and text after the caret.
    pub fn segments(&self) -> (&str, &str, &str) {
        (&self.text[..self.cursor], &self.preedit, &self.text[self.cursor..])
    }

    /// The caret rectangle in physical px, if the drawing code recorded one.
    pub fn caret_area(&self) -> Option<(i32, i32, i32, i32)> {
        self.caret_area
    }

    /// Record where the caret is on screen so the host can place the IME
    /// candidate window next to it (physical px).
    pub fn set_caret_area(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.caret_area = Some((x, y, w, h));
    }

    // ── Editing ─────────────────────────────────────────────────────────────

    /// Insert one character at the caret, honoring the char limit and skipping
    /// control characters. Returns whether anything was inserted.
    fn insert_char(&mut self, c: char) -> bool {
        if c.is_control() || self.char_count() >= self.max_chars {
            return false;
        }
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        true
    }

    /// Insert a string at the caret (used for IME commits and clipboard paste).
    pub fn insert_str(&mut self, s: &str) -> bool {
        let mut edited = false;
        for c in s.chars() {
            if !self.insert_char(c) {
                break;
            }
            edited = true;
        }
        edited
    }

    /// Delete the character before the caret.
    fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .expect("non-empty prefix");
        let start = self.cursor - prev.len_utf8();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        true
    }

    /// Delete the character after the caret.
    fn delete(&mut self) -> bool {
        if self.cursor >= self.text.len() {
            return false;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .expect("non-empty suffix");
        let end = self.cursor + next.len_utf8();
        self.text.replace_range(self.cursor..end, "");
        true
    }

    fn move_left(&mut self) {
        if let Some(prev) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= prev.len_utf8();
        }
    }

    fn move_right(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    // ── Event handling ──────────────────────────────────────────────────────

    /// Process a key event. Returns whether the event was consumed (so the
    /// caller can stop routing it). Navigation, editing and direct (non-IME)
    /// character input are handled here; the caller still owns Enter/Esc/Tab
    /// and history keys.
    pub fn handle_key(
        &mut self,
        event: &KeyEvent,
        modifiers: ModifiersState,
        clipboard: Option<&mut arboard::Clipboard>,
    ) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }

        // Command combos (Ctrl on Linux/Windows, Cmd on macOS): only paste is
        // handled; everything else is left for the caller / OS.
        if modifiers.control_key() || modifiers.super_key() {
            if matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyV)) {
                if let Some(text) = clipboard.and_then(|c| c.get_text().ok()) {
                    // Paste is a single line; drop any newlines.
                    let sanitized: String =
                        text.chars().filter(|c| !c.is_control()).collect();
                    self.insert_str(&sanitized);
                }
                return true;
            }
            return false;
        }

        match event.physical_key {
            PhysicalKey::Code(KeyCode::Backspace) => self.backspace(),
            PhysicalKey::Code(KeyCode::Delete) => self.delete(),
            PhysicalKey::Code(KeyCode::ArrowLeft) => {
                self.move_left();
                true
            }
            PhysicalKey::Code(KeyCode::ArrowRight) => {
                self.move_right();
                true
            }
            PhysicalKey::Code(KeyCode::Home) => {
                self.cursor = 0;
                true
            }
            PhysicalKey::Code(KeyCode::End) => {
                self.cursor = self.text.len();
                true
            }
            _ => {
                // While the IME is composing, keystrokes belong to it; ignore
                // any character winit might still surface (defensive — macOS
                // does not deliver one during a preedit).
                if self.is_composing() {
                    return false;
                }
                // `event.text` covers letters, digits, space and punctuation
                // (space arrives as a named key with text " ", not a Character).
                match &event.text {
                    Some(text) => self.insert_str(text),
                    None => false,
                }
            }
        }
    }

    /// Process an IME event: update the composition preview, or commit the
    /// composed text into the buffer at the caret.
    pub fn handle_ime(&mut self, ime: &Ime) -> bool {
        match ime {
            Ime::Enabled | Ime::Disabled => {
                self.preedit.clear();
                true
            }
            Ime::Preedit(text, _cursor) => {
                self.preedit.clear();
                self.preedit.push_str(text);
                true
            }
            Ime::Commit(text) => {
                self.preedit.clear();
                self.insert_str(text);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_and_backspaces_unicode() {
        let mut input = TextInput::new(16);
        input.insert_str("ab");
        input.handle_ime(&Ime::Commit("你好".to_owned()));
        assert_eq!(input.text(), "ab你好");
        assert_eq!(input.char_count(), 4);
        // Backspace removes a whole multi-byte char.
        input.backspace();
        assert_eq!(input.text(), "ab你");
    }

    #[test]
    fn caret_movement_inserts_in_the_middle() {
        let mut input = TextInput::with_text(16, "ac");
        input.move_left();
        assert!(input.insert_str("b"));
        assert_eq!(input.text(), "abc");
    }

    #[test]
    fn respects_char_limit() {
        let mut input = TextInput::new(2);
        assert!(input.insert_str("ab"));
        assert!(!input.insert_str("c"));
        assert_eq!(input.text(), "ab");
    }

    #[test]
    fn preedit_is_separate_until_committed() {
        let mut input = TextInput::new(16);
        input.handle_ime(&Ime::Preedit("ni".to_owned(), None));
        assert!(input.is_composing());
        assert_eq!(input.text(), ""); // not yet committed
        assert_eq!(input.segments().1, "ni"); // shown inline as the preview
        input.handle_ime(&Ime::Commit("你".to_owned()));
        assert!(!input.is_composing());
        assert_eq!(input.text(), "你");
    }
}
