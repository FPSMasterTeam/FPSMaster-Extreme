//! Lightweight internationalization, modeled on vanilla's `Locale`: a flat
//! `key -> value` table parsed from the vanilla `.lang` files in the active
//! assets, with `%s`/`%d`/`%n$s` argument substitution. The active language is
//! layered over the always-present `en_US` base so a partial translation falls
//! back per-key, exactly like vanilla. FPSMaster-specific keys (menus, screens
//! that have no vanilla equivalent) ship in an embedded overlay so they still
//! translate for the languages we maintain, and read as English everywhere else.

use std::collections::BTreeMap;
use std::sync::{LazyLock, RwLock};

/// The default and fallback language; the only `.lang` shipped inside the
/// vanilla jar, so it is always available.
pub const DEFAULT_LANG: &str = "en_US";

/// FPSMaster-specific strings (keys absent from the vanilla tables), one embedded
/// overlay per language we maintain. Layered over the vanilla `.lang`.
const OVERLAY_EN: &str = include_str!("lang/fpsmaster.en_us.lang");
const OVERLAY_ZH: &str = include_str!("lang/fpsmaster.zh_cn.lang");

fn overlay_for(code: &str) -> &'static str {
    match code {
        "zh_CN" => OVERLAY_ZH,
        _ => OVERLAY_EN,
    }
}

struct Locale {
    code: String,
    table: BTreeMap<String, String>,
    /// Reverse index `vanilla en_US name -> translation key`, restricted to the
    /// name namespaces (`tile.`/`item.`/`enchantment.`/`attribute.name.`) and to
    /// values that map to a single key. Lets [`localize_name`] translate the
    /// hardcoded English block/item/enchantment/attribute names that fpsmaster
    /// carries (it has no numeric-id → key registry) back through the table.
    names: BTreeMap<String, String>,
}

static LOCALE: LazyLock<RwLock<Locale>> = LazyLock::new(|| {
    let base = load_en_base();
    RwLock::new(Locale {
        code: DEFAULT_LANG.to_owned(),
        names: build_name_index(&base),
        table: base,
    })
});

/// A selectable language for the language screen.
pub struct LangInfo {
    pub code: String,
    /// Native display name (`English`, `简体中文`), from `language.name`.
    pub name: String,
    /// Country/region label (`US`, `中国`), from `language.region`.
    pub region: String,
}

fn lang_asset(code: &str) -> String {
    format!("assets/minecraft/lang/{code}.lang")
}

/// Parse a `.lang` file (`key=value` per line, `#` comments, blank lines) into
/// `table`, overriding any existing keys.
fn parse_lang(text: &str, table: &mut BTreeMap<String, String>) {
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            table.insert(key.to_owned(), value.to_owned());
        }
    }
}

/// The always-English base: the vanilla `en_US` table plus the English overlay.
/// Used both as the fallback layer and as the source for the name index.
fn load_en_base() -> BTreeMap<String, String> {
    let mut table = BTreeMap::new();
    if let Some(text) = crate::read_asset_string(&lang_asset(DEFAULT_LANG)) {
        parse_lang(&text, &mut table);
    }
    parse_lang(OVERLAY_EN, &mut table);
    table
}

/// Build the lookup table for `code`: the English base (so missing keys fall
/// back), then the requested language and its overlay on top.
fn load_table(code: &str) -> BTreeMap<String, String> {
    let mut table = load_en_base();
    if code != DEFAULT_LANG {
        if let Some(text) = crate::read_asset_string(&lang_asset(code)) {
            parse_lang(&text, &mut table);
        }
        parse_lang(overlay_for(code), &mut table);
    }
    table
}

/// Build the `English name -> key` reverse index from the English base, keeping
/// only name-namespace keys whose value is unique (ambiguous names are left
/// untranslated by [`localize_name`] rather than guessed).
fn build_name_index(en_base: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let is_name_key = |key: &str| {
        key.starts_with("tile.")
            || key.starts_with("item.")
            || key.starts_with("enchantment.")
            || key.starts_with("attribute.name.")
            || key.starts_with("potion.")
    };
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for (key, value) in en_base {
        if is_name_key(key) {
            *counts.entry(value.as_str()).or_insert(0) += 1;
        }
    }
    let mut index = BTreeMap::new();
    for (key, value) in en_base {
        if is_name_key(key) && counts.get(value.as_str()) == Some(&1) {
            index.insert(value.clone(), key.clone());
        }
    }
    index
}

/// Switch the active language and rebuild the lookup table. Loads from the
/// active assets, so it must be called after the asset path is configured.
pub fn set_language(code: &str) {
    let table = load_table(code);
    let names = build_name_index(&load_en_base());
    let mut locale = LOCALE.write().unwrap();
    locale.code = code.to_owned();
    locale.table = table;
    locale.names = names;
}

/// Translate a hardcoded vanilla English block/item/enchantment/attribute name
/// to the active language by resolving it back to its translation key. Returns
/// the input unchanged for names without a unique key (custom/derived names).
pub fn localize_name(english: &str) -> String {
    let locale = LOCALE.read().unwrap();
    match locale.names.get(english) {
        Some(key) => locale.table.get(key).cloned().unwrap_or_else(|| english.to_owned()),
        None => english.to_owned(),
    }
}

/// The active language code (`en_US`, `zh_CN`, …).
pub fn current_language() -> String {
    LOCALE.read().unwrap().code.clone()
}

/// Look up `key`, returning `None` if it is not in the active table.
pub fn translate_opt(key: &str) -> Option<String> {
    LOCALE.read().unwrap().table.get(key).cloned()
}

/// Translate `key`, falling back to the key itself when unknown (vanilla
/// behavior for menu strings).
pub fn tr(key: &str) -> String {
    translate_opt(key).unwrap_or_else(|| key.to_owned())
}

/// Translate `key` and substitute its `%s`/`%d`/`%n$s` placeholders with `args`.
pub fn tr_args(key: &str, args: &[&str]) -> String {
    format_args_str(&tr(key), args)
}

/// Substitute Java-style format placeholders in `pattern`: `%s`/`%d` consume
/// the next positional argument in order, `%n$s` selects argument `n` (1-based),
/// and `%%` is a literal percent. Unknown/extra specifiers consume an argument
/// silently; missing arguments expand to nothing.
pub fn format_args_str(pattern: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars().peekable();
    let mut next = 0usize;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some(d) if d.is_ascii_digit() => {
                let mut num = String::new();
                while let Some(d) = chars.peek() {
                    if d.is_ascii_digit() {
                        num.push(*d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if chars.peek() == Some(&'$') {
                    chars.next();
                }
                chars.next(); // conversion char (s/d/…)
                if let Some(arg) = num
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| args.get(i))
                {
                    out.push_str(arg);
                }
            }
            Some(_) => {
                chars.next(); // conversion char
                if let Some(arg) = args.get(next) {
                    out.push_str(arg);
                }
                next += 1;
            }
            None => out.push('%'),
        }
    }
    out
}

/// Every language available in the active assets, with its native name/region,
/// sorted by display name. Backs the language-selection screen.
pub fn available_languages() -> Vec<LangInfo> {
    let mut list: Vec<LangInfo> = crate::list_lang_codes()
        .into_iter()
        .map(|code| {
            let mut meta = BTreeMap::new();
            if let Some(text) = crate::read_asset_string(&lang_asset(&code)) {
                parse_lang(&text, &mut meta);
            }
            let name = meta
                .get("language.name")
                .cloned()
                .unwrap_or_else(|| code.clone());
            let region = meta.get("language.region").cloned().unwrap_or_default();
            LangInfo { code, name, region }
        })
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_sequential_and_positional() {
        assert_eq!(format_args_str("<%s> %s", &["Steve", "hi"]), "<Steve> hi");
        assert_eq!(
            format_args_str("%2$s then %1$s", &["a", "b"]),
            "b then a"
        );
        assert_eq!(format_args_str("100%% done", &[]), "100% done");
        assert_eq!(format_args_str("%d chunks", &["12"]), "12 chunks");
    }
}

