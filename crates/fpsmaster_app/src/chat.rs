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

/// One received chat line: the `§`-coded text (what the HUD renders) plus the
/// structured segments preserving click/hover events for the open chat screen.
#[derive(Debug, Clone)]
pub struct ChatLine {
    pub text: String,
    pub segments: Vec<ChatSegment>,
    pub received: Instant,
}

/// A click action carried by a chat component's `clickEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickAction {
    /// Run the value as a chat command (it already includes the leading `/`).
    RunCommand,
    /// Pre-fill the chat input with the value.
    SuggestCommand,
    /// Open the value as a URL in the OS browser.
    OpenUrl,
    /// Copy the value to the system clipboard.
    CopyToClipboard,
}

/// A chat component's `clickEvent`: an action plus its string value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickEvent {
    pub action: ClickAction,
    pub value: String,
}

/// One run of chat text with its legacy `§` style codes, plus the optional
/// click/hover events the open chat screen acts on. `text` carries the same
/// `§` codes the HUD renderer and width helpers expect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSegment {
    pub text: String,
    pub click: Option<ClickEvent>,
    /// Flattened `§`-coded hover text (`hoverEvent` show_text), parsed and
    /// stored but not yet rendered as a tooltip.
    pub hover: Option<String>,
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
    /// Last server tab-complete candidates and the cursor into them for the
    /// cycle-on-repeated-Tab behaviour.
    completions: Vec<String>,
    completion_cursor: usize,
}

impl ChatState {
    /// Append a received chat/system line that is plain (no click/hover).
    pub fn push_message(&mut self, text: String) {
        let segment = ChatSegment {
            text: text.clone(),
            click: None,
            hover: None,
        };
        self.push_line(text, vec![segment]);
    }

    /// Append a received chat line built from structured segments (preserving
    /// the click/hover events the open chat screen acts on).
    pub fn push_components(&mut self, segments: Vec<ChatSegment>) {
        let text = segments.iter().map(|s| s.text.as_str()).collect();
        self.push_line(text, segments);
    }

    fn push_line(&mut self, text: String, segments: Vec<ChatSegment>) {
        self.lines.push_back(ChatLine {
            text,
            segments,
            received: Instant::now(),
        });
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    /// The backlog lines, oldest first.
    pub fn lines(&self) -> &VecDeque<ChatLine> {
        &self.lines
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

    /// Store fresh tab-complete candidates (resets the cycle cursor).
    pub fn set_completions(&mut self, matches: Vec<String>) {
        self.completions = matches;
        self.completion_cursor = 0;
    }

    /// The next completion candidate to apply, advancing the cycle cursor so a
    /// repeated Tab walks through the list. `None` when there are no candidates.
    pub fn next_completion(&mut self) -> Option<&str> {
        if self.completions.is_empty() {
            return None;
        }
        let index = self.completion_cursor % self.completions.len();
        self.completion_cursor += 1;
        Some(&self.completions[index])
    }

    /// Clear stored candidates (the input changed, so a stale cycle must not
    /// keep applying old matches).
    pub fn clear_completions(&mut self) {
        self.completions.clear();
        self.completion_cursor = 0;
    }

    pub fn has_completions(&self) -> bool {
        !self.completions.is_empty()
    }
}

/// Replace the last whitespace-separated token of `text` with `completion`
/// (vanilla GuiChat tab-completion). The caret is implicitly at the end of the
/// single-line chat box. An empty or whitespace-trailing input appends the
/// completion as a new token.
pub fn complete_word(text: &str, completion: &str) -> String {
    let token_start = text.rfind(' ').map_or(0, |i| i + 1);
    let mut out = text[..token_start].to_owned();
    out.push_str(completion);
    out
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

/// Parse a JSON chat component into structured segments that preserve the
/// `clickEvent` / `hoverEvent` of each run (children inherit a parent's events
/// unless they declare their own). The concatenated segment `text` equals
/// [`flatten_chat_json`]. Falls back to a single plain segment for non-JSON.
pub fn parse_chat_components(json: &str) -> Vec<ChatSegment> {
    match serde_json::from_str::<Value>(json) {
        Ok(value) => {
            let mut out = Vec::new();
            append_segments(&value, None, None, &mut out);
            out
        }
        Err(_) => vec![ChatSegment {
            text: json.to_owned(),
            click: None,
            hover: None,
        }],
    }
}

fn append_segments(
    value: &Value,
    click: Option<&ClickEvent>,
    hover: Option<&str>,
    out: &mut Vec<ChatSegment>,
) {
    match value {
        Value::String(s) => push_segment(s.clone(), click, hover, out),
        Value::Array(parts) => {
            for part in parts {
                append_segments(part, click, hover, out);
            }
        }
        Value::Object(obj) => {
            // A component's own events override the inherited ones for itself
            // and its children.
            let own_click = parse_click_event(obj);
            let own_hover = parse_hover_text(obj);
            let click = own_click.as_ref().or(click);
            let hover = own_hover.as_deref().or(hover);

            let mut text = style_codes(obj);
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
                text.push_str(&format_translation(key, &args));
            } else if let Some(t) = obj.get("text").and_then(Value::as_str) {
                text.push_str(t);
            }
            push_segment(text, click, hover, out);
            if let Some(Value::Array(extra)) = obj.get("extra") {
                for part in extra {
                    append_segments(part, click, hover, out);
                }
            }
        }
        _ => {}
    }
}

fn push_segment(
    text: String,
    click: Option<&ClickEvent>,
    hover: Option<&str>,
    out: &mut Vec<ChatSegment>,
) {
    if text.is_empty() {
        return;
    }
    out.push(ChatSegment {
        text,
        click: click.cloned(),
        hover: hover.map(str::to_owned),
    });
}

/// Parse a component's `clickEvent` object into a [`ClickEvent`]. Unknown
/// actions (e.g. `change_page`) are dropped.
fn parse_click_event(obj: &serde_json::Map<String, Value>) -> Option<ClickEvent> {
    let event = obj.get("clickEvent")?.as_object()?;
    let value = event.get("value").and_then(Value::as_str)?.to_owned();
    let action = match event.get("action").and_then(Value::as_str)? {
        "run_command" => ClickAction::RunCommand,
        "suggest_command" => ClickAction::SuggestCommand,
        "open_url" => ClickAction::OpenUrl,
        "copy_to_clipboard" => ClickAction::CopyToClipboard,
        _ => return None,
    };
    Some(ClickEvent { action, value })
}

/// Parse a component's `hoverEvent` (only `show_text` is supported) into
/// flattened `§`-coded text.
fn parse_hover_text(obj: &serde_json::Map<String, Value>) -> Option<String> {
    let event = obj.get("hoverEvent")?.as_object()?;
    if event.get("action").and_then(Value::as_str) != Some("show_text") {
        return None;
    }
    let value = event.get("value")?;
    let mut text = String::new();
    append_component(value, &mut text);
    Some(text)
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

/// Resolve a server-sent translation component against the active language
/// table (vanilla `%s`/`%n$s` patterns). Unknown keys fall back to the joined
/// arguments (or the key itself when there are none) — readable even without a
/// matching entry.
fn format_translation(key: &str, args: &[String]) -> String {
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match crate::i18n::translate_opt(key) {
        Some(pattern) => crate::i18n::format_args_str(&pattern, &argv),
        None => {
            if args.is_empty() {
                key.to_owned()
            } else {
                args.join(" ")
            }
        }
    }
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
    let max_width = max_width.max(fpsmaster_render::char_width('W', scale));
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
        let advance = fpsmaster_render::char_width(c, scale);
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
            line_width = fpsmaster_render::text_width(&line, scale);
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

/// A clickable horizontal span on one wrapped chat row: the x range (in screen
/// px, relative to the row's text start) and the click event to fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickSpan {
    pub x_start: i32,
    pub x_end: i32,
    pub event: ClickEvent,
}

/// Lay out `segments` with the same greedy word-wrap as [`wrap_legacy`] and
/// return, per visual row (first row topmost), the clickable x-spans. Used by
/// the open chat screen to map a click position back to a segment's action.
pub fn clickable_spans(segments: &[ChatSegment], max_width: i32, scale: i32) -> Vec<Vec<ClickSpan>> {
    let max_width = max_width.max(fpsmaster_render::char_width('W', scale));

    // Flatten to a stream of visible chars, each tagged with the index of its
    // owning segment (so a wrap break can recompute carried-char positions).
    struct Glyph {
        c: char,
        seg: usize,
    }
    let mut glyphs: Vec<Glyph> = Vec::new();
    for (seg, segment) in segments.iter().enumerate() {
        let mut chars = segment.text.chars();
        while let Some(c) = chars.next() {
            if c == '§' {
                chars.next();
            } else {
                glyphs.push(Glyph { c, seg });
            }
        }
    }

    // Greedily fill rows, recording each row's glyph range, then convert the
    // ranges into per-segment x-spans below.
    let mut rows: Vec<Vec<&Glyph>> = Vec::new();
    let mut row: Vec<&Glyph> = Vec::new();
    let mut width = 0;
    let mut last_space: Option<usize> = None; // index into `row`
    for glyph in &glyphs {
        if glyph.c == '\n' {
            rows.push(std::mem::take(&mut row));
            width = 0;
            last_space = None;
            continue;
        }
        let advance = fpsmaster_render::char_width(glyph.c, scale);
        if width + advance > max_width && width > 0 {
            if glyph.c == ' ' {
                rows.push(std::mem::take(&mut row));
                width = 0;
                last_space = None;
                continue;
            }
            let carried: Vec<&Glyph> = match last_space {
                Some(at) => {
                    let mut rest = row.split_off(at);
                    while rest.first().is_some_and(|g| g.c == ' ') {
                        rest.remove(0);
                    }
                    rest
                }
                None => Vec::new(),
            };
            rows.push(std::mem::take(&mut row));
            row = carried;
            width = row.iter().map(|g| fpsmaster_render::char_width(g.c, scale)).sum();
            last_space = None;
        }
        if glyph.c == ' ' {
            last_space = Some(row.len());
        }
        row.push(glyph);
        width += advance;
    }
    rows.push(row);

    rows.into_iter()
        .map(|row| {
            let mut spans: Vec<ClickSpan> = Vec::new();
            let mut x = 0;
            for glyph in row {
                let advance = fpsmaster_render::char_width(glyph.c, scale);
                if let Some(event) = segments[glyph.seg].click.clone() {
                    // Extend the running span when this glyph continues the same
                    // segment's clickable run; otherwise start a new span.
                    match spans.last_mut() {
                        Some(last) if last.x_end == x && last.event == event => {
                            last.x_end = x + advance;
                        }
                        _ => spans.push(ClickSpan {
                            x_start: x,
                            x_end: x + advance,
                            event,
                        }),
                    }
                }
                x += advance;
            }
            spans
        })
        .collect()
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
    fn parse_components_extracts_run_command_and_plain_text() {
        let json = r#"{"text":"hi ","extra":[{"text":"[click]","color":"green","clickEvent":{"action":"run_command","value":"/spawn"}}]}"#;
        let segments = parse_chat_components(json);
        // Concatenated text matches the flattened form.
        let joined: String = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, flatten_chat_json(json));
        // The plain run carries no click; the bracket run carries the command.
        assert_eq!(segments[0].click, None);
        assert_eq!(
            segments[1].click,
            Some(ClickEvent {
                action: ClickAction::RunCommand,
                value: "/spawn".to_owned(),
            })
        );
    }

    #[test]
    fn parse_components_children_inherit_and_keep_hover() {
        let json = r#"{"text":"a","hoverEvent":{"action":"show_text","value":"tip"},"clickEvent":{"action":"suggest_command","value":"/msg x "},"extra":[{"text":"b"}]}"#;
        let segments = parse_chat_components(json);
        let suggest = Some(ClickEvent {
            action: ClickAction::SuggestCommand,
            value: "/msg x ".to_owned(),
        });
        assert_eq!(segments[0].click, suggest);
        assert_eq!(segments[0].hover.as_deref(), Some("tip"));
        // The child with no events of its own inherits the parent's.
        assert_eq!(segments[1].click, suggest);
        assert_eq!(segments[1].hover.as_deref(), Some("tip"));
    }

    #[test]
    fn complete_word_replaces_last_token() {
        assert_eq!(complete_word("/te", "/tell"), "/tell");
        assert_eq!(complete_word("/tell Ste", "Steve"), "/tell Steve");
        assert_eq!(complete_word("", "/help"), "/help");
        // A trailing space starts a fresh token.
        assert_eq!(complete_word("/tell ", "Steve"), "/tell Steve");
    }

    #[test]
    fn clickable_spans_locate_the_clicked_segment() {
        let segments = vec![
            ChatSegment {
                text: "hi ".to_owned(),
                click: None,
                hover: None,
            },
            ChatSegment {
                text: "[go]".to_owned(),
                click: Some(ClickEvent {
                    action: ClickAction::RunCommand,
                    value: "/go".to_owned(),
                }),
                hover: None,
            },
        ];
        let rows = clickable_spans(&segments, 10_000, 1);
        assert_eq!(rows.len(), 1);
        let span = &rows[0][0];
        // The clickable run starts after "hi " and covers "[go]".
        assert_eq!(span.x_start, fpsmaster_render::text_width("hi ", 1));
        assert_eq!(span.x_end, fpsmaster_render::text_width("hi [go]", 1));
        assert_eq!(span.event.value, "/go");
    }

    #[test]
    fn wrap_breaks_at_spaces_and_carries_color() {
        // Wrap width = exactly "one two": the second space ends the line and
        // the continuation re-emits the active color.
        let max = fpsmaster_render::text_width("one two", 1);
        let lines = wrap_legacy("§aone two three", max, 1);
        assert_eq!(lines, vec!["§aone two".to_owned(), "§athree".to_owned()]);
    }

    #[test]
    fn wrap_hard_breaks_long_words() {
        let max = fpsmaster_render::text_width("aaaa", 1);
        let lines = wrap_legacy("aaaaaaaaaa", max, 1);
        assert_eq!(lines, vec!["aaaa", "aaaa", "aa"]);
    }

    #[test]
    fn wrapped_lines_fit_the_requested_width() {
        let max = fpsmaster_render::text_width("some words", 2);
        let text = "§b一段 mixed width 文本 with spaces and a veryverylongunbrokenword here";
        for line in wrap_legacy(text, max, 2) {
            assert!(
                fpsmaster_render::text_width(&line, 2) <= max,
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
