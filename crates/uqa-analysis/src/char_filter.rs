//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Character-level filters that run before tokenization.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CharFilter {
    HTMLStrip,
    Mapping {
        mapping: BTreeMap<String, String>,
    },
    PatternReplace {
        pattern: String,
        #[serde(default)]
        replacement: String,
    },
}

impl CharFilter {
    pub fn filter(&self, text: &str) -> String {
        match self {
            CharFilter::HTMLStrip => {
                let stripped = html_tag_re().replace_all(text, " ").into_owned();
                replace_entities(&stripped)
            }
            CharFilter::Mapping { mapping } => {
                let ordered = mapping_longest_first(mapping);
                let mut out = text.to_owned();
                for (old, new) in ordered {
                    out = out.replace(&old, &new);
                }
                out
            }
            CharFilter::PatternReplace {
                pattern,
                replacement,
            } => match Regex::new(pattern) {
                Ok(re) => re.replace_all(text, replacement.as_str()).into_owned(),
                Err(_) => text.to_owned(),
            },
        }
    }
}

fn html_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("html tag regex compiles"))
}

const HTML_ENTITIES: &[(&str, &str)] = &[
    ("&amp;", "&"),
    ("&lt;", "<"),
    ("&gt;", ">"),
    ("&quot;", "\""),
    ("&#39;", "'"),
    ("&apos;", "'"),
    ("&nbsp;", " "),
];

fn replace_entities(text: &str) -> String {
    let mut out = text.to_owned();
    for (entity, replacement) in HTML_ENTITIES {
        out = out.replace(entity, replacement);
    }
    out
}

/// Order mapping entries longest-key-first so that, e.g., the rule
/// `aa -> X` fires before `a -> Y`.
fn mapping_longest_first(m: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> =
        m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_strip_removes_tags_and_decodes_entities() {
        let f = CharFilter::HTMLStrip;
        assert_eq!(
            f.filter("<p>hello &amp; world</p>"),
            " hello & world ".to_string()
        );
    }

    #[test]
    fn mapping_replaces_longest_first() {
        // Longest-first ordering: `aa` consumes the prefix before the
        // single-`a` rule sees it, leaving nothing for the second rule.
        // Without longest-first ordering the single-char rule would fire
        // twice and produce "YYb".
        let mut m = BTreeMap::new();
        m.insert("aa".to_string(), "X".to_string());
        m.insert("a".to_string(), "Y".to_string());
        let f = CharFilter::Mapping { mapping: m };
        assert_eq!(f.filter("aab"), "Xb");

        // A 'a' that wasn't in the longer rule's match still gets replaced
        // by the shorter rule.
        assert_eq!(f.filter("aba"), "YbY");
    }

    #[test]
    fn pattern_replace_uses_regex() {
        let f = CharFilter::PatternReplace {
            pattern: r"\d+".to_string(),
            replacement: "#".to_string(),
        };
        assert_eq!(f.filter("a1b22c"), "a#b#c");
    }
}
