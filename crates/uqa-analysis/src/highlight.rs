//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Search-result highlighting.
//!
//! Explicit analyzers process the complete source and query inputs, then highlight matching lossless terms at their original source offsets. Overlapping source spans are merged when rendering, so compound alternatives and rewritten terms do not produce nested markers.
//!
//! Without an analyzer, the helper scans Unicode words and compares their lower-case forms. The word-scanning entry point also preserves the existing SQL highlighting behavior.
//!
//! ```rust
//! use uqa_analysis::{highlight, HighlightOptions};
//!
//! let out = highlight(
//!     "the quick brown fox jumps over the lazy dog",
//!     &["fox".into(), "dog".into()],
//!     None,
//!     &HighlightOptions::default(),
//! ).unwrap();
//! assert!(out.contains("<b>fox</b>"));
//! assert!(out.contains("<b>dog</b>"));
//! ```

use crate::{AnalysisResult, Analyzer};
use uqa_core::memory::{Budgeted, MemoryBudget};

/// Per-call configuration. Defaults use `<b>` / `</b>` tags, a full-text
/// highlight with no fragment cap, and
/// 150-char fragments when `max_fragments > 0`.
#[derive(Debug, Clone)]
pub struct HighlightOptions {
    pub start_tag: String,
    pub end_tag: String,
    /// `0` keeps the whole text and just wraps matches; `> 0`
    /// extracts that many fragments centred on the densest match
    /// clusters.
    pub max_fragments: usize,
    pub fragment_size: usize,
}

impl Default for HighlightOptions {
    fn default() -> Self {
        Self {
            start_tag: "<b>".into(),
            end_tag: "</b>".into(),
            max_fragments: 0,
            fragment_size: 150,
        }
    }
}

mod ordering;
mod render;
mod rich;
mod terms;
mod words;
pub use rich::{highlight_compiled, highlight_compiled_budgeted};
pub use words::highlight_words_budgeted;

/// Highlight complete source spans with an explicit analyzer, or lowercase word matches when omitted.
pub fn highlight(
    text: &str,
    query_terms: &[String],
    analyzer: Option<&Analyzer>,
    opts: &HighlightOptions,
) -> AnalysisResult<String> {
    Ok(highlight_budgeted(
        text,
        query_terms,
        analyzer,
        opts,
        &MemoryBudget::new(usize::MAX),
        || Ok(()),
    )?
    .into_parts()
    .0)
}

/// Highlight with one allowance for analysis, matching, fragment selection, and returned text.
///
/// The caller owns input text, query strings, options, and immutable preparation resources. Owned runtime buffers retain their reservations until destruction. Allocation-limit and callback failures return no partial string. An explicit analyzer is compiled once; configured library searches execute between callback checks.
///
/// ```
/// use uqa_analysis::{highlight_budgeted, HighlightOptions};
/// use uqa_core::memory::MemoryBudget;
/// let budget = MemoryBudget::new(64 * 1024);
/// let result = highlight_budgeted("the quick fox", &["FOX".into()], None, &HighlightOptions::default(), &budget, || Ok(()))?;
/// assert_eq!(&**result, "the quick <b>fox</b>");
/// assert_eq!(budget.used(), result.reserved_bytes());
/// drop(result);
/// assert_eq!(budget.used(), 0);
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
pub fn highlight_budgeted(
    text: &str,
    query_terms: &[String],
    analyzer: Option<&Analyzer>,
    opts: &HighlightOptions,
    budget: &MemoryBudget,
    mut poll: impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    poll()?;
    if text.is_empty() || query_terms.is_empty() {
        return crate::allocation::copy_text(text, budget, &mut poll);
    }
    if let Some(analyzer) = analyzer {
        let compiled = analyzer.compile()?;
        highlight_compiled_budgeted(text, query_terms, &compiled, opts, budget, poll)
    } else {
        highlight_words_budgeted(
            text,
            query_terms.iter().map(String::as_str),
            None,
            opts,
            budget,
            poll,
        )
    }
}

/// Highlight independently analyzed source words, retaining the legacy word-scanning contract.
pub fn highlight_words(
    text: &str,
    query_terms: &[String],
    analyzer: Option<&Analyzer>,
    opts: &HighlightOptions,
) -> AnalysisResult<String> {
    Ok(highlight_words_budgeted(
        text,
        query_terms.iter().map(String::as_str),
        analyzer,
        opts,
        &MemoryBudget::new(usize::MAX),
        || Ok(()),
    )?
    .into_parts()
    .0)
}

#[cfg(test)]
mod tests;
