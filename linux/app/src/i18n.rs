//! User interface translation. Source strings are English; the French catalog (`po/fr.po`,
//! gettext syntax) is embedded at build time and used when the system language is French.
//! No `.mo` file to install, no gettext dependency. Daemon messages and the channel names it
//! publishes stay English.
//!
//! Format strings use named placeholders (`{name}`), filled by [`trf`]: a translation may
//! reorder them, and the tests check that every translation keeps the same placeholders.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;

const FR: &str = include_str!("../po/fr.po");

static CATALOG: OnceLock<HashMap<String, String>> = OnceLock::new();

fn catalog() -> &'static HashMap<String, String> {
    CATALOG.get_or_init(|| {
        // Unit tests compare English texts, whatever the language of the machine.
        if !cfg!(test) && french(&|k| std::env::var(k).ok()) {
            parse(FR).unwrap_or_default()
        } else {
            HashMap::new()
        }
    })
}

/// Language of messages, as gettext picks it: `LC_ALL`, then `LC_MESSAGES`, then `LANG`
/// (first non-empty value); French for `fr`, `fr_FR.UTF-8`, `fr_CA`…
fn french(var: &dyn Fn(&str) -> Option<String>) -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| var(k).filter(|v| !v.is_empty()))
        .is_some_and(|v| v == "fr" || v.starts_with("fr_") || v.starts_with("fr."))
}

/// French catalog, for tests of other modules.
#[cfg(test)]
pub fn french_catalog() -> HashMap<String, String> {
    parse(FR).unwrap_or_default()
}

/// Translation of `msgid`, or `msgid` itself.
pub fn tr(msgid: &'static str) -> &'static str {
    match catalog().get(msgid) {
        Some(s) => s.as_str(),
        None => msgid,
    }
}

/// Translation of `msgid` with its `{name}` placeholders replaced by `args`.
pub fn trf(msgid: &'static str, args: &[(&str, &dyn Display)]) -> String {
    fill(tr(msgid), args)
}

fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

/// Parses a gettext catalog: `msgid` / `msgstr` pairs, strings continued on following lines,
/// `\"`, `\\`, `\n` and `\t` escapes. Untranslated entries (empty `msgstr`) and the header are
/// skipped.
fn parse(po: &str) -> Result<HashMap<String, String>, String> {
    #[derive(PartialEq)]
    enum Field {
        None,
        Id,
        Str,
    }
    let mut map = HashMap::new();
    let (mut id, mut text, mut field) = (String::new(), String::new(), Field::None);
    let mut flush = |id: &mut String, text: &mut String| {
        if !id.is_empty() && !text.is_empty() {
            map.insert(std::mem::take(id), std::mem::take(text));
        }
        id.clear();
        text.clear();
    };
    for (n, raw) in po.lines().enumerate() {
        let line = raw.trim();
        let bad = |what: &str| format!("fr.po line {}: {what}", n + 1);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("msgid ") {
            if field == Field::Str {
                flush(&mut id, &mut text);
            } else if field == Field::Id {
                return Err(bad("msgid without msgstr"));
            }
            id = unquote(rest).ok_or_else(|| bad("invalid string"))?;
            field = Field::Id;
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            if field != Field::Id {
                return Err(bad("msgstr without msgid"));
            }
            text = unquote(rest).ok_or_else(|| bad("invalid string"))?;
            field = Field::Str;
        } else if line.starts_with('"') {
            let more = unquote(line).ok_or_else(|| bad("invalid string"))?;
            match field {
                Field::Id => id.push_str(&more),
                Field::Str => text.push_str(&more),
                Field::None => return Err(bad("string outside an entry")),
            }
        } else {
            return Err(bad("unsupported keyword"));
        }
    }
    if field == Field::Id {
        return Err("fr.po: last msgid without msgstr".into());
    }
    flush(&mut id, &mut text);
    Ok(map)
}

/// Content of a quoted gettext string.
fn unquote(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(match chars.next()? {
                'n' => '\n',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                _ => return None,
            }),
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(s: &str) -> BTreeSet<String> {
        let mut set = BTreeSet::new();
        let mut rest = s;
        while let Some(start) = rest.find('{') {
            let tail = &rest[start + 1..];
            match tail.find('}') {
                Some(end) => {
                    set.insert(tail[..end].to_string());
                    rest = &tail[end + 1..];
                }
                None => break,
            }
        }
        set
    }

    #[test]
    fn catalog_parses_and_keeps_placeholders() {
        let map = parse(FR).expect("fr.po");
        assert!(map.len() > 100, "{} entries", map.len());
        for (id, text) in &map {
            assert_eq!(placeholders(id), placeholders(text), "{id:?} → {text:?}");
            // French punctuation: no plain space before a double sign.
            for sign in [" :", " ;", " !", " ?", "« ", " »"] {
                assert!(!text.contains(sign), "plain space in {text:?} ({sign:?})");
            }
            assert!(!text.contains('"'), "straight quotes in {text:?}");
        }
        assert_eq!(map["Cancel"], "Annuler");
    }

    /// Literal arguments of `tr("…")` and `trf("…", …)` calls in `src`.
    fn msgids(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for call in ["tr(", "trf("] {
            for (i, _) in src.match_indices(call) {
                let ident = src[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
                let rest = src[i + call.len()..].trim_start();
                let Some(lit) = rest.strip_prefix('"') else {
                    continue;
                };
                if ident {
                    continue;
                }
                let mut s = String::new();
                let mut chars = lit.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some(c) => s.push(c),
                            None => break,
                        },
                        c => s.push(c),
                    }
                }
                out.push(s);
            }
        }
        out
    }

    #[test]
    fn every_string_is_translated() {
        let map = parse(FR).expect("fr.po");
        let sources = [
            include_str!("window.rs"),
            include_str!("grid.rs"),
            include_str!("client.rs"),
            include_str!("listener.rs"),
            include_str!("models.rs"),
        ];
        let ids: Vec<String> = sources.iter().flat_map(|s| msgids(s)).collect();
        assert!(ids.len() > 100, "{} calls found", ids.len());
        let missing: Vec<&String> = ids.iter().filter(|id| !map.contains_key(*id)).collect();
        assert!(missing.is_empty(), "missing in fr.po: {missing:#?}");
    }

    #[test]
    fn syntax() {
        let map = parse(
            "# comment\nmsgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain\\n\"\n\n\
             msgid \"A \\\"b\\\"\"\nmsgstr \"\"\n\"C\"\n\"D\"\n\nmsgid \"untranslated\"\nmsgstr \"\"\n",
        )
        .unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map["A \"b\""], "CD");
        assert!(parse("msgid \"a\"\nmsgid \"b\"\nmsgstr \"c\"\n").is_err());
        assert!(parse("msgstr \"c\"\n").is_err());
        assert!(parse("msgid \"a\n").is_err());
        assert!(parse("msgctxt \"a\"\n").is_err());
    }

    #[test]
    fn language() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(french(&env(&[("LANG", "fr_FR.UTF-8")])));
        assert!(french(&env(&[("LANG", "fr_CA")])));
        assert!(french(&env(&[("LC_ALL", ""), ("LANG", "fr")])));
        assert!(!french(&env(&[("LC_ALL", "C"), ("LANG", "fr_FR.UTF-8")])));
        assert!(!french(&env(&[
            ("LC_MESSAGES", "en_US.UTF-8"),
            ("LANG", "fr_FR.UTF-8")
        ])));
        assert!(french(&env(&[
            ("LC_MESSAGES", "fr_BE.UTF-8"),
            ("LANG", "en_US")
        ])));
        assert!(!french(&env(&[("LANG", "fy_NL")])));
        assert!(!french(&env(&[])));
    }

    #[test]
    fn fills_named_placeholders() {
        assert_eq!(
            fill("{b} then {a}, {a}", &[("a", &1), ("b", &"x")]),
            "x then 1, 1"
        );
    }
}
