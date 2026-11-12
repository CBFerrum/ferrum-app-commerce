//! The storefront's words. The script (`assets/store.js`) has none of its own: each page the server writes
//! carries the words of one language as JSON, and the script's `t("key")` reads them.
//!
//! * **Files.** `assets/words/en.json` is the app's English, every word the script shows by key. Another
//!   language the app ships is another file beside it (`SHIPPED`); a workspace gives or overrides any
//!   language's words in **Storefront Words** (one record a language, a JSON object of key to text), which
//!   needs no release. English is under every language: a key a language does not have is the English one.
//! * **Which language.** The store's languages are Store Settings' (the first is the default). A visitor's own
//!   choice (`?lang=`) comes first, then what the browser asks for (`Accept-Language`, by preference), each
//!   matched to an offered language by its code, else by its first part (`de-AT` takes `de`); else the default.

use async_trait::async_trait;
use ferrum_domain::util::s;
use ferrum_kernel::{Doc, DocHook, Error, FieldError, HookCtx, Result, Tx};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::sync::OnceLock;

pub const ENGLISH: &str = include_str!("../assets/words/en.json");
/// The languages the app ships words for besides what a workspace writes: code and file.
const SHIPPED: [(&str, &str); 1] = [("en", ENGLISH)];
/// Languages written right to left, by their first part.
const RTL: [&str; 9] = ["ar", "he", "fa", "ur", "ps", "sd", "yi", "dv", "ku"];

/// The English words, by key.
pub fn english() -> &'static Map<String, Value> {
    static EN: OnceLock<Map<String, Value>> = OnceLock::new();
    EN.get_or_init(|| serde_json::from_str(ENGLISH).expect("assets/words/en.json is a JSON object of words"))
}

/// A language code as the store lists it: letters and digits in parts of at most 8, as `en`, `pt-BR`.
pub fn valid_code(code: &str) -> bool {
    let parts: Vec<&str> = code.split('-').collect();
    !code.is_empty()
        && code.len() <= 16
        && parts.iter().all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|c| c.is_ascii_alphanumeric()))
        && parts[0].len() >= 2
        && parts[0].bytes().all(|c| c.is_ascii_alphabetic())
}

/// The store's languages from its setting: codes separated by commas, lower case; none given, English.
pub fn offered(setting: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for code in setting.unwrap_or_default().split([',', ';', ' ']).map(|c| c.trim().to_lowercase()).filter(|c| valid_code(c)) {
        if !out.contains(&code) {
            out.push(code);
        }
    }
    if out.is_empty() {
        out.push("en".into());
    }
    out
}

/// What a browser asks for, in the order it prefers (`de-AT,de;q=0.9,en;q=0.5`): codes in lower case, those it
/// refuses (`q=0`) left out.
pub fn accepted(header: &str) -> Vec<String> {
    let mut asked: Vec<(f32, usize, String)> = header
        .split(',')
        .enumerate()
        .filter_map(|(i, part)| {
            let mut bits = part.split(';');
            let code = bits.next()?.trim().to_lowercase();
            let q = bits.find_map(|b| b.trim().strip_prefix("q=").and_then(|v| v.trim().parse::<f32>().ok())).unwrap_or(1.0);
            (valid_code(&code) && q > 0.0).then_some((q, i, code))
        })
        .collect();
    asked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    asked.into_iter().map(|(_, _, c)| c).collect()
}

/// The language a page is written in: the first of `asked` the store offers (by its code, else by its first
/// part), else the store's default.
pub fn choose(offered: &[String], asked: &[String]) -> String {
    let first = |c: &str| c.split('-').next().unwrap_or(c).to_string();
    for want in asked {
        if let Some(exact) = offered.iter().find(|o| *o == want) {
            return exact.clone();
        }
        if let Some(near) = offered.iter().find(|o| first(o) == first(want)) {
            return near.clone();
        }
    }
    offered.first().cloned().unwrap_or_else(|| "en".into())
}

/// Whether a language is written right to left.
pub fn right_to_left(code: &str) -> bool {
    RTL.contains(&code.split('-').next().unwrap_or(code))
}

/// A page's words in one language: the English, the language's own shipped words over it, the workspace's over those.
pub async fn merged(tx: &mut Tx, ctx: &ferrum_kernel::Ctx, lang: &str) -> Result<Map<String, Value>> {
    let mut out = english().clone();
    let mut lay = |words: &Map<String, Value>| {
        for (key, text) in words {
            if out.contains_key(key) && text.is_string() {
                out.insert(key.clone(), text.clone());
            }
        }
    };
    for (code, file) in SHIPPED {
        if code == lang
            && code != "en"
            && let Ok(Value::Object(words)) = serde_json::from_str::<Value>(file)
        {
            lay(&words);
        }
    }
    // A workspace that has not had the update has no table for them: its words are the app's.
    let own: Option<Value> = if ctx.schema.entity("StorefrontWords").is_some() {
        sqlx::query_scalar("select words from e_storefront_words where tenant_id = $1 and lower(language) = $2")
            .bind(ctx.tenant)
            .bind(lang)
            .fetch_optional(&mut **tx)
            .await?
            .flatten()
    } else {
        None
    };
    if let Some(Value::Object(words)) = own {
        lay(&words);
    }
    Ok(out)
}

/// The places a word fills (`{name}`), in order of first appearance.
fn places(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else { break };
        let name = &rest[start + 1..start + end];
        if !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            out.insert(name.to_string());
        }
        rest = &rest[start + end + 1..];
    }
    out
}

/// What is wrong with a language's words, as the first thing to fix: a key the storefront does not use (a typo
/// would silently change nothing), a word that is not text, one that names a place its English has not (it would
/// show as typed), or one too long.
pub fn problem(words: &Value) -> Option<String> {
    let Value::Object(map) = words else { return Some("must be a JSON object of key to text".into()) };
    for (key, text) in map {
        let Some(base) = english().get(key).and_then(Value::as_str) else { return Some(format!("{key} is not a word the storefront uses")) };
        let Some(text) = text.as_str() else { return Some(format!("{key} must be text")) };
        if text.chars().count() > 600 {
            return Some(format!("{key} is longer than 600 characters"));
        }
        if let Some(extra) = places(text).difference(&places(base)).next() {
            return Some(format!("{key} names {{{extra}}}, which its English does not fill"));
        }
    }
    None
}

/// Storefront Words: a workspace's own words for a language.
pub struct StorefrontWordsHook;

#[async_trait]
impl DocHook for StorefrontWordsHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if let Some(code) = s(doc, "language").map(|c| c.trim().to_lowercase()) {
            doc.insert("language".into(), Value::String(code));
        }
        Ok(())
    }

    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut errors = Vec::new();
        if !s(doc, "language").is_some_and(valid_code) {
            errors.push(FieldError::new("language", "use a language code such as en, hi or pt-br"));
        }
        if let Some(why) = doc.get("words").filter(|w| !w.is_null()).and_then(problem) {
            errors.push(FieldError::new("words", why));
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn codes(v: &[&str]) -> Vec<String> {
        v.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn the_store_lists_its_languages() {
        assert_eq!(offered(None), codes(&["en"]));
        assert_eq!(offered(Some("")), codes(&["en"]));
        assert_eq!(offered(Some("hi, EN; ar hi, x, de-AT")), codes(&["hi", "en", "ar", "de-at"]), "lower case, once each, codes only");
        assert!(valid_code("pt-br") && valid_code("en") && !valid_code("e") && !valid_code("en_US") && !valid_code("1n") && !valid_code("en-"));
    }

    #[test]
    fn a_visitor_gets_what_they_asked_for_that_the_store_offers() {
        let store = codes(&["en", "hi", "de"]);
        assert_eq!(accepted("de-AT,de;q=0.9,en;q=0.5, fr;q=0"), codes(&["de-at", "de", "en"]));
        assert_eq!(accepted("en;q=0.4, hi;q=0.9"), codes(&["hi", "en"]), "by preference, not by place");
        assert_eq!(choose(&store, &codes(&["hi-in", "en"])), "hi", "by the first part when the code is not listed");
        assert_eq!(choose(&store, &codes(&["fr", "de"])), "de", "the first the store has");
        assert_eq!(choose(&store, &codes(&["fr"])), "en", "else the default, the first listed");
        assert_eq!(choose(&store, &[]), "en");
        assert_eq!(choose(&codes(&["hi", "en"]), &codes(&["fr"])), "hi", "the default is the store's first");
        assert!(right_to_left("ar") && right_to_left("he-il") && !right_to_left("en"));
    }

    #[test]
    fn a_language_may_only_change_words_that_exist_and_keep_their_places() {
        assert_eq!(problem(&json!({"cart.empty": "Khaali", "cart.count_aria": "{n} mein"})), None);
        assert!(problem(&json!({"cart.emtpy": "x"})).unwrap().contains("not a word"));
        assert!(problem(&json!({"cart.empty": 3})).unwrap().contains("must be text"));
        assert!(problem(&json!({"cart.count_aria": "{count} in"})).unwrap().contains("{count}"), "a place its English does not fill");
        assert!(problem(&json!(["x"])).is_some());
    }

    #[test]
    fn every_word_of_the_english_is_used_by_the_script_and_every_use_has_a_word() {
        // The code, not what its comments say about it.
        let script: String = include_str!("../assets/store.js")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("/**"))
            .collect::<Vec<_>>()
            .join("\n");
        let plural = ["zero", "one", "two", "few", "many", "other"];
        let known = |key: &str| english().contains_key(key) || plural.iter().any(|p| english().contains_key(&format!("{key}.{p}")));
        // Every word the script asks for by name is in the English file.
        for call in ["t(\"", "tn(\""] {
            for (at, _) in script.match_indices(call).filter(|(at, _)| !script[..*at].ends_with(|c: char| c.is_alphanumeric() || c == '_')) {
                let key: String = script[at + call.len()..].chars().take_while(|c| *c != '"').collect();
                assert!(known(&key), "the script asks for {key}, which the English file lacks");
            }
        }
        // Every word of the English file is asked for somewhere (a key named in a choice, or the label of a field a code names).
        let literals: BTreeSet<&str> = script.split('"').skip(1).step_by(2).collect();
        for key in english().keys() {
            let base = plural.iter().find_map(|p| key.strip_suffix(&format!(".{p}"))).unwrap_or(key);
            let by_field = key.starts_with("label.") && script.contains("t(`label.${");
            assert!(literals.contains(key.as_str()) || literals.contains(base) || by_field, "{key} is in the English file and no script asks for it");
        }
    }
}
