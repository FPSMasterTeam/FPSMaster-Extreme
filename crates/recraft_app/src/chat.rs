//! Chat state and vanilla JSON-chat handling: incoming components are
//! flattened to a plain string carrying legacy `§` style codes (the form the
//! HUD renderer consumes), and the chat box keeps the receive/send history
//! the UI needs (visible lines with fade, input recall, action bar).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::Value;

/// The 1.8 server kicks for chat messages longer than this.
pub const MAX_CHAT_INPUT: usize = 100;
/// How long a received line stays visible while the chat is closed.
const LINE_VISIBLE: Duration = Duration::from_secs(10);
/// Fade-out window at the end of a line's visible time.
const LINE_FADE: Duration = Duration::from_secs(1);
/// Lines kept in memory (open-chat backlog).
const MAX_LINES: usize = 100;
/// Action-bar display time (vanilla: 60 ticks).
const ACTION_BAR_VISIBLE: Duration = Duration::from_secs(3);

/// One received chat line, already flattened to `§`-coded text.
#[derive(Debug, Clone)]
pub struct ChatLine {
    pub text: String,
    pub received: Instant,
}

#[derive(Debug, Default)]
pub struct ChatState {
    /// Received lines, newest at the back.
    lines: VecDeque<ChatLine>,
    /// The action-bar text (chat position 2) and when it was set.
    action_bar: Option<(String, Instant)>,
    /// Messages the player sent, oldest first, for up/down recall.
    sent: Vec<String>,
    /// Recall cursor: `sent.len()` means "fresh line".
    sent_cursor: usize,
}

impl ChatState {
    /// Append a received chat/system line.
    pub fn push_message(&mut self, text: String) {
        self.lines.push_back(ChatLine {
            text,
            received: Instant::now(),
        });
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    pub fn set_action_bar(&mut self, text: String) {
        self.action_bar = Some((text, Instant::now()));
    }

    /// The action-bar text with its current fade alpha, if still visible.
    pub fn action_bar(&self, now: Instant) -> Option<(&str, f32)> {
        let (text, since) = self.action_bar.as_ref()?;
        let age = now.saturating_duration_since(*since);
        if age >= ACTION_BAR_VISIBLE {
            return None;
        }
        let remaining = (ACTION_BAR_VISIBLE - age).as_secs_f32();
        Some((
            text.as_str(),
            (remaining / LINE_FADE.as_secs_f32()).min(1.0),
        ))
    }

    /// Lines to draw, newest first, each with its fade alpha. With the chat
    /// open every backlog line shows at full opacity; closed, only lines
    /// younger than 10 s show, fading over their last second.
    pub fn visible_lines(&self, now: Instant, open: bool) -> Vec<(&str, f32)> {
        let mut out = Vec::new();
        for line in self.lines.iter().rev() {
            if open {
                out.push((line.text.as_str(), 1.0));
                continue;
            }
            let age = now.saturating_duration_since(line.received);
            if age >= LINE_VISIBLE {
                break; // older lines are older still
            }
            let remaining = (LINE_VISIBLE - age).as_secs_f32();
            out.push((
                line.text.as_str(),
                (remaining / LINE_FADE.as_secs_f32()).min(1.0),
            ));
        }
        out
    }

    /// Record a sent message and reset the recall cursor.
    pub fn record_sent(&mut self, message: String) {
        if self.sent.last() != Some(&message) {
            self.sent.push(message);
        }
        self.sent_cursor = self.sent.len();
    }

    /// Reset the recall cursor (called when the chat box opens).
    pub fn reset_recall(&mut self) {
        self.sent_cursor = self.sent.len();
    }

    /// Recall the previous sent message (ArrowUp).
    pub fn recall_previous(&mut self) -> Option<String> {
        if self.sent_cursor == 0 {
            return None;
        }
        self.sent_cursor -= 1;
        self.sent.get(self.sent_cursor).cloned()
    }

    /// Recall the next sent message, or an empty line past the newest (ArrowDown).
    pub fn recall_next(&mut self) -> Option<String> {
        if self.sent_cursor >= self.sent.len() {
            return None;
        }
        self.sent_cursor += 1;
        Some(self.sent.get(self.sent_cursor).cloned().unwrap_or_default())
    }
}

// ─── JSON chat component flattening ─────────────────────────────────────────

/// Flatten a JSON chat component to plain text with legacy `§` codes. Falls
/// back to the raw string when it isn't valid JSON (some servers send bare
/// legacy text).
pub fn flatten_chat_json(json: &str) -> String {
    match serde_json::from_str::<Value>(json) {
        Ok(value) => {
            let mut out = String::new();
            append_component(&value, &mut out);
            out
        }
        Err(_) => json.to_owned(),
    }
}

fn append_component(value: &Value, out: &mut String) {
    match value {
        Value::String(s) => out.push_str(s),
        Value::Array(parts) => {
            for part in parts {
                append_component(part, out);
            }
        }
        Value::Object(obj) => {
            out.push_str(&style_codes(obj));
            if let Some(key) = obj.get("translate").and_then(Value::as_str) {
                let args: Vec<String> = obj
                    .get("with")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .map(|part| {
                                let mut s = String::new();
                                append_component(part, &mut s);
                                s
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push_str(&format_translation(key, &args));
            } else if let Some(text) = obj.get("text").and_then(Value::as_str) {
                out.push_str(text);
            }
            if let Some(Value::Array(extra)) = obj.get("extra") {
                for part in extra {
                    append_component(part, out);
                }
            }
        }
        _ => {}
    }
}

/// Legacy codes for a component's explicit style. Color codes implicitly clear
/// formatting (vanilla legacy semantics), so emit the color first.
fn style_codes(obj: &serde_json::Map<String, Value>) -> String {
    let mut codes = String::new();
    if let Some(color) = obj.get("color").and_then(Value::as_str) {
        if let Some(code) = color_name_to_code(color) {
            codes.push('§');
            codes.push(code);
        }
    }
    for (key, code) in [
        ("obfuscated", 'k'),
        ("bold", 'l'),
        ("strikethrough", 'm'),
        ("underlined", 'n'),
        ("italic", 'o'),
    ] {
        if obj.get(key).and_then(Value::as_bool) == Some(true) {
            codes.push('§');
            codes.push(code);
        }
    }
    codes
}

fn color_name_to_code(name: &str) -> Option<char> {
    Some(match name {
        "black" => '0',
        "dark_blue" => '1',
        "dark_green" => '2',
        "dark_aqua" => '3',
        "dark_red" => '4',
        "dark_purple" => '5',
        "gold" => '6',
        "gray" => '7',
        "dark_gray" => '8',
        "blue" => '9',
        "green" => 'a',
        "aqua" => 'b',
        "red" => 'c',
        "light_purple" => 'd',
        "yellow" => 'e',
        "white" => 'f',
        "reset" => 'r',
        _ => return None,
    })
}

/// The translation strings a client actually sees in normal play. Unknown keys
/// fall back to the joined arguments (or the key itself when there are none) —
/// readable even without the full language table.
fn format_translation(key: &str, args: &[String]) -> String {
    let pattern = match key {
        "chat.type.text" => "<{0}> {1}",
        "chat.type.announcement" => "[{0}] {1}",
        "chat.type.emote" => "* {0} {1}",
        "chat.type.admin" => "[{0}: {1}]",
        "multiplayer.player.joined" => "{0} joined the game",
        "multiplayer.player.left" => "{0} left the game",
        "commands.message.display.incoming" => "{0} whispers to you: {1}",
        "commands.message.display.outgoing" => "You whisper to {0}: {1}",
        _ => {
            return if args.is_empty() {
                key.to_owned()
            } else {
                args.join(" ")
            };
        }
    };
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut index = String::new();
            for d in chars.by_ref() {
                if d == '}' {
                    break;
                }
                index.push(d);
            }
            if let Some(arg) = index.parse::<usize>().ok().and_then(|i| args.get(i)) {
                out.push_str(arg);
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ─── Legacy § code utilities (shared with the HUD renderer) ─────────────────

/// Whether a `§` code selects one of the 16 colors.
fn is_color_code(code: char) -> bool {
    code.is_ascii_hexdigit()
}

/// Strip `§x` codes, leaving only visible characters.
pub fn strip_legacy_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// Greedy word-wrap of `§`-coded text to at most `max_width` screen px per
/// line at `scale`, using the real font advances. Continuation lines re-emit
/// the active color code so colors survive the break (vanilla GuiNewChat
/// behaviour).
pub fn wrap_legacy(text: &str, max_width: i32, scale: i32) -> Vec<String> {
    // Never wrap tighter than one of the widest glyphs.
    let max_width = max_width.max(recraft_render::char_width('W', scale));
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    let mut last_space: Option<usize> = None; // byte index into `line`
    let mut active_color: Option<char> = None;

    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '§' {
            if let Some(&code) = chars.peek() {
                chars.next();
                line.push('§');
                line.push(code);
                if is_color_code(code) || code == 'r' || code == 'R' {
                    active_color = Some(code);
                }
            }
            continue;
        }
        if c == '\n' {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
            last_space = None;
            if let Some(code) = active_color {
                line.push('§');
                line.push(code);
            }
            continue;
        }
        let advance = recraft_render::char_width(c, scale);
        if line_width + advance > max_width && line_width > 0 {
            if c == ' ' {
                // The overflowing character is itself a space: end the line
                // here and drop it.
                lines.push(std::mem::take(&mut line));
                if let Some(code) = active_color {
                    line.push('§');
                    line.push(code);
                }
                line_width = 0;
                last_space = None;
                continue;
            }
            // Break at the last space when there is one, else hard-break.
            let carried = if let Some(at) = last_space {
                let rest = line[at..].trim_start_matches(' ').to_owned();
                line.truncate(at);
                rest
            } else {
                String::new()
            };
            lines.push(std::mem::take(&mut line));
            if let Some(code) = active_color {
                line.push('§');
                line.push(code);
            }
            line.push_str(&carried);
            line_width = recraft_render::text_width(&line, scale);
            last_space = None;
        }
        if c == ' ' {
            last_space = Some(line.len());
        }
        line.push(c);
        line_width += advance;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_component_flattens() {
        assert_eq!(flatten_chat_json(r#"{"text":"hello"}"#), "hello");
        assert_eq!(flatten_chat_json(r#""bare string""#), "bare string");
        assert_eq!(flatten_chat_json("not json"), "not json");
    }

    #[test]
    fn extra_and_colors_flatten_to_legacy_codes() {
        let json = r#"{"text":"A","extra":[{"text":"B","color":"red"},{"text":"C","color":"green","bold":true}]}"#;
        assert_eq!(flatten_chat_json(json), "A§cB§a§lC");
    }

    #[test]
    fn translate_chat_type_text_formats_like_vanilla() {
        let json = r#"{"translate":"chat.type.text","with":[{"text":"Steve"},{"text":"hi all"}]}"#;
        assert_eq!(flatten_chat_json(json), "<Steve> hi all");
    }

    #[test]
    fn unknown_translate_falls_back_to_args() {
        let json = r#"{"translate":"death.attack.player","with":["Alice","Bob"]}"#;
        assert_eq!(flatten_chat_json(json), "Alice Bob");
    }

    #[test]
    fn strip_ignores_codes() {
        assert_eq!(strip_legacy_codes("§a§lhi §cthere"), "hi there");
    }

    #[test]
    fn wrap_breaks_at_spaces_and_carries_color() {
        // Wrap width = exactly "one two": the second space ends the line and
        // the continuation re-emits the active color.
        let max = recraft_render::text_width("one two", 1);
        let lines = wrap_legacy("§aone two three", max, 1);
        assert_eq!(lines, vec!["§aone two".to_owned(), "§athree".to_owned()]);
    }

    #[test]
    fn wrap_hard_breaks_long_words() {
        let max = recraft_render::text_width("aaaa", 1);
        let lines = wrap_legacy("aaaaaaaaaa", max, 1);
        assert_eq!(lines, vec!["aaaa", "aaaa", "aa"]);
    }

    #[test]
    fn wrapped_lines_fit_the_requested_width() {
        let max = recraft_render::text_width("some words", 2);
        let text = "§b一段 mixed width 文本 with spaces and a veryverylongunbrokenword here";
        for line in wrap_legacy(text, max, 2) {
            assert!(
                recraft_render::text_width(&line, 2) <= max,
                "line {line:?} exceeds the wrap width"
            );
        }
    }

    #[test]
    fn recall_walks_history_both_ways() {
        let mut chat = ChatState::default();
        chat.record_sent("first".to_owned());
        chat.record_sent("second".to_owned());
        chat.reset_recall();
        assert_eq!(chat.recall_previous().as_deref(), Some("second"));
        assert_eq!(chat.recall_previous().as_deref(), Some("first"));
        assert_eq!(chat.recall_previous(), None);
        assert_eq!(chat.recall_next().as_deref(), Some("second"));
        assert_eq!(chat.recall_next().as_deref(), Some(""));
        assert_eq!(chat.recall_next(), None);
    }

    #[test]
    fn visible_lines_fade_and_expire() {
        let mut chat = ChatState::default();
        chat.push_message("fresh".to_owned());
        let now = Instant::now();
        let visible = chat.visible_lines(now, false);
        assert_eq!(visible.len(), 1);
        assert!(visible[0].1 > 0.99);
        // Pretend the line is 9.5s old: it should be fading.
        chat.lines[0].received = now - Duration::from_millis(9500);
        let visible = chat.visible_lines(now, false);
        assert!(visible[0].1 < 0.6);
        // At 11s it is gone when closed, but still listed when open.
        chat.lines[0].received = now - Duration::from_secs(11);
        assert!(chat.visible_lines(now, false).is_empty());
        assert_eq!(chat.visible_lines(now, true).len(), 1);
    }
}
