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

use crate::error::{AnalysisError, AnalysisResult};
use crate::FilteredText;
use uqa_core::memory::MemoryBudget;

mod compiled;
mod replacement;
mod stream;
pub(crate) use compiled::PreparedCharFilter;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CharFilter {
    // The alias keeps catalogs persisted before the stable tag existed
    // deserializable: releases up to 0.1.2 wrote the derived spelling.
    #[serde(rename = "html_strip", alias = "h_t_m_l_strip")]
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
    /// Validate configuration without filtering input.
    pub fn validate(&self) -> AnalysisResult<()> {
        match self {
            CharFilter::PatternReplace { .. } => self.prepare().map(|_| ()),
            _ => Ok(()),
        }
    }

    pub fn filter(&self, text: &str) -> AnalysisResult<String> {
        Ok(self.filter_with_offsets(text)?.into_string())
    }

    /// Transform text while retaining source coordinates for the result.
    pub fn filter_with_offsets<'a>(&self, text: &'a str) -> AnalysisResult<FilteredText<'a>> {
        self.filter_mapped(FilteredText::new(text))
    }

    /// Apply this stage to previously filtered text without losing its original source.
    pub fn filter_mapped<'a>(&self, text: FilteredText<'a>) -> AnalysisResult<FilteredText<'a>> {
        self.prepare()?.filter_mapped(text)
    }

    /// Transform a borrowed input while retaining source buffers under the caller's byte allowance. Immutable configuration preparation is separate. Polls occur between searches and during source copying and coordinate construction.
    ///
    /// ```
    /// use uqa_analysis::CharFilter;
    /// use uqa_core::memory::MemoryBudget;
    /// let budget = MemoryBudget::new(16 * 1024);
    /// let filtered = CharFilter::HTMLStrip.filter_with_offsets_budgeted(
    ///     "<b>한&amp;🙂</b>", &budget, &mut || Ok(()),
    /// )?;
    /// let retained = filtered.clone();
    /// drop(filtered);
    /// assert_eq!(retained.as_str(), " 한&🙂 ");
    /// assert_eq!(retained.source_offsets(1..4)?.utf8, 3..6);
    /// assert!(budget.used() > 0);
    /// drop(retained);
    /// assert_eq!(budget.used(), 0);
    /// # Ok::<(), uqa_analysis::AnalysisError>(())
    /// ```
    pub fn filter_with_offsets_budgeted<'a>(
        &self,
        text: &'a str,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<FilteredText<'a>> {
        self.filter_mapped_budgeted(FilteredText::new(text), budget, poll)
    }

    /// New source, edit, and coordinate buffers use `budget`; retained input allocations keep their original shared leases.
    pub fn filter_mapped_budgeted<'a>(
        &self,
        text: FilteredText<'a>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<FilteredText<'a>> {
        poll()?;
        self.prepare()?.filter_mapped_budgeted(text, budget, poll)
    }
}

fn html_tag_re() -> AnalysisResult<&'static Regex> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]+>").map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| AnalysisError::BuiltInRegex {
            component: "HTML tag filter",
            message: message.clone(),
        })
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
            f.filter("<p>hello &amp; world</p>").unwrap(),
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
        assert_eq!(f.filter("aab").unwrap(), "Xb");

        // A 'a' that wasn't in the longer rule's match still gets replaced
        // by the shorter rule.
        assert_eq!(f.filter("aba").unwrap(), "YbY");
    }

    #[test]
    fn pattern_replace_uses_regex() {
        let f = CharFilter::PatternReplace {
            pattern: r"\d+".to_string(),
            replacement: "#".to_string(),
        };
        assert_eq!(f.filter("a1b22c").unwrap(), "a#b#c");
    }
}
