//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Search-result highlighting.
//!
//! Highlighting operates in two phases:
//!
//! 1. Build a set of *analyzed* query terms (lower-cased + stemmed +
//!    char/token filtered through the same [`Analyzer`] pipeline used
//!    for indexing). When the caller does not supply an analyzer, the
//!    fallback is a plain ASCII lower-case fold so the highlighter
//!    still works as a stand-alone helper.
//! 2. Walk the source text with a `\w+` tokenizer; every token whose
//!    analyzed form intersects the query-term set becomes a highlight
//!    span. Spans are wrapped with the configured `start_tag` /
//!    `end_tag`, or projected into a fragment view when
//!    `max_fragments > 0`.
//!
//! The matcher operates on character offsets rather than byte offsets, so
//! highlight spans align correctly in CJK and other multibyte text.
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

#![allow(
    clippy::similar_names,
    clippy::explicit_counter_loop,
    clippy::needless_range_loop,
    clippy::stable_sort_primitive,
    clippy::manual_midpoint,
    clippy::map_unwrap_or
)]

use std::collections::BTreeSet;

use regex::Regex;

use crate::analyzer::Analyzer;
use crate::error::AnalysisResult;

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

fn word_regex() -> AnalysisResult<&'static Regex> {
    use std::sync::OnceLock;
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\w+").map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| crate::error::AnalysisError::BuiltInRegex {
            component: "highlighter word scanner",
            message: message.clone(),
        })
}

/// Wrap matched query terms in `text` with the configured tags.
///
/// `analyzer` is optional: when supplied, both the query terms and
/// the source text are run through the same pipeline so stemming /
/// lower-casing / accent folding agree. When omitted, ASCII
/// lower-case is used instead.
pub fn highlight(
    text: &str,
    query_terms: &[String],
    analyzer: Option<&Analyzer>,
    opts: &HighlightOptions,
) -> AnalysisResult<String> {
    if text.is_empty() || query_terms.is_empty() {
        return Ok(text.to_string());
    }

    let analyzed: BTreeSet<String> = match analyzer {
        Some(a) => {
            let mut analyzed = BTreeSet::new();
            for query_term in query_terms {
                analyzed.extend(a.analyze(query_term)?);
            }
            analyzed
        }
        None => query_terms.iter().map(|qt| qt.to_lowercase()).collect(),
    };

    if analyzed.is_empty() {
        return Ok(text.to_string());
    }

    // Walk the text once, collecting (char_start, char_end) spans
    // for every token whose analyzed form intersects the query-term
    // set. Char offsets are tracked alongside the regex byte offsets
    // so the highlight wrappers slice correctly on multi-byte text.
    //
    // `byte_to_char[byte_idx]` is the char count of the prefix
    // ending at `byte_idx`. The last entry maps `text.len()` to the
    // total char count so a regex match end past the final byte
    // still maps cleanly.
    let total_chars = text.chars().count();
    let mut byte_to_char: Vec<usize> = vec![0usize; text.len() + 1];
    {
        let mut last_byte = 0usize;
        let mut last_char = 0usize;
        for (byte_idx, _) in text.char_indices() {
            for slot in last_byte..=byte_idx {
                byte_to_char[slot] = last_char;
            }
            last_byte = byte_idx + 1;
            last_char += 1;
        }
        for slot in last_byte..byte_to_char.len() {
            byte_to_char[slot] = total_chars;
        }
    }
    let to_char = |byte: usize| -> usize {
        if byte >= byte_to_char.len() {
            total_chars
        } else {
            byte_to_char[byte]
        }
    };

    let mut match_spans: Vec<(usize, usize)> = Vec::new();
    for m in word_regex()?.find_iter(text) {
        let token = m.as_str();
        let hit = match analyzer {
            Some(a) => {
                let toks = a.analyze(token)?;
                !toks.is_empty() && toks.iter().any(|t| analyzed.contains(t))
            }
            None => analyzed.contains(&token.to_lowercase()),
        };
        if hit {
            match_spans.push((to_char(m.start()), to_char(m.end())));
        }
    }

    if match_spans.is_empty() {
        if opts.max_fragments > 0 {
            return Ok(ellipsis_prefix(text, opts.fragment_size));
        }
        return Ok(text.to_string());
    }

    let highlighted = if opts.max_fragments > 0 {
        build_fragments(text, &match_spans, opts)
    } else {
        wrap_full(text, &match_spans, &opts.start_tag, &opts.end_tag)
    };
    Ok(highlighted)
}

fn ellipsis_prefix(text: &str, fragment_size: usize) -> String {
    let total = text.chars().count();
    let take = fragment_size.min(total);
    let mut out = String::new();
    out.extend(text.chars().take(take));
    if take < total {
        out.push_str("...");
    }
    out
}

/// Splice `start_tag` / `end_tag` into `text` around every char-offset
/// span in `match_spans`. Spans are assumed to be in left-to-right
/// order.
fn wrap_full(text: &str, match_spans: &[(usize, usize)], start_tag: &str, end_tag: &str) -> String {
    // Convert char offsets back to byte boundaries via a single
    // pass over the source.
    let char_to_byte: Vec<usize> = {
        let mut v: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
        v.push(text.len());
        v
    };
    let to_byte = |c: usize| -> usize {
        if c >= char_to_byte.len() {
            text.len()
        } else {
            char_to_byte[c]
        }
    };
    let mut out = String::with_capacity(text.len());
    let mut prev_byte = 0usize;
    for (cs, ce) in match_spans {
        let bs = to_byte(*cs);
        let be = to_byte(*ce);
        out.push_str(&text[prev_byte..bs]);
        out.push_str(start_tag);
        out.push_str(&text[bs..be]);
        out.push_str(end_tag);
        prev_byte = be;
    }
    out.push_str(&text[prev_byte..]);
    out
}

fn build_fragments(text: &str, match_spans: &[(usize, usize)], opts: &HighlightOptions) -> String {
    let half = (opts.fragment_size / 2).max(1);
    let total_chars = text.chars().count();

    // Group nearby matches into clusters: a span joins the current
    // cluster if it starts within `half` characters of the previous
    // cluster's right edge.
    let mut clusters: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut current: Vec<(usize, usize)> = Vec::new();
    for &span in match_spans {
        if current.is_empty() {
            current.push(span);
            continue;
        }
        let last_end = current.last().map_or(span.0, |previous| previous.1);
        if span.0.saturating_sub(last_end) > half {
            clusters.push(std::mem::take(&mut current));
            current.push(span);
        } else {
            current.push(span);
        }
    }
    if !current.is_empty() {
        clusters.push(current);
    }

    // Pick the densest clusters, then put the survivors back in
    // textual order so the resulting string reads left-to-right.
    clusters.sort_by_key(|c| std::cmp::Reverse(c.len()));
    let mut selected: Vec<Vec<(usize, usize)>> =
        clusters.into_iter().take(opts.max_fragments).collect();
    selected.sort_by_key(|c| c[0].0);

    // Convert the picked clusters into bounded text windows.
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let chars_len = chars.len();
    let char_at = |idx: usize| -> usize {
        if idx >= chars_len {
            text.len()
        } else {
            chars[idx].0
        }
    };
    let char_range_to_string = |start: usize, end: usize| -> String {
        let bs = char_at(start);
        let be = if end >= chars_len {
            text.len()
        } else {
            chars[end].0
        };
        text[bs..be].to_string()
    };

    let mut fragments: Vec<String> = Vec::new();
    for cluster in selected {
        let (Some(first), Some(last)) = (cluster.first(), cluster.last()) else {
            continue;
        };
        let centre = first.0 + last.1.saturating_sub(first.0) / 2;
        let mut frag_start = centre.saturating_sub(half);
        let mut frag_end = (centre + half).min(total_chars);

        // Snap to nearest space boundary so we do not bisect a word.
        if frag_start > 0 {
            let mut probe = frag_start;
            let limit = (frag_start + 30).min(total_chars);
            while probe < limit {
                if chars
                    .get(probe)
                    .map(|(_, c)| c.is_whitespace())
                    .unwrap_or(false)
                {
                    frag_start = probe + 1;
                    break;
                }
                probe += 1;
            }
        }
        if frag_end < total_chars {
            let lower = frag_end.saturating_sub(30);
            let mut probe = frag_end;
            while probe > lower {
                if chars
                    .get(probe - 1)
                    .map(|(_, c)| c.is_whitespace())
                    .unwrap_or(false)
                {
                    frag_end = probe - 1;
                    break;
                }
                probe -= 1;
            }
        }

        let frag_text = char_range_to_string(frag_start, frag_end);
        let local_spans: Vec<(usize, usize)> = cluster
            .iter()
            .filter(|(s, e)| *s >= frag_start && *e <= frag_end)
            .map(|(s, e)| (s - frag_start, e - frag_start))
            .collect();
        let highlighted = wrap_full(&frag_text, &local_spans, &opts.start_tag, &opts.end_tag);

        let prefix = if frag_start > 0 { "..." } else { "" };
        let suffix = if frag_end < total_chars { "..." } else { "" };
        fragments.push(format!("{prefix}{highlighted}{suffix}"));
    }
    fragments.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_matched_terms_with_default_tags() {
        let out = highlight(
            "the quick brown fox",
            &["fox".into(), "quick".into()],
            None,
            &HighlightOptions::default(),
        )
        .unwrap();
        assert_eq!(out, "the <b>quick</b> brown <b>fox</b>");
    }

    #[test]
    fn returns_text_unchanged_when_no_query_terms() {
        let out = highlight("untouched", &[], None, &HighlightOptions::default()).unwrap();
        assert_eq!(out, "untouched");
    }

    #[test]
    fn returns_text_unchanged_when_no_matches() {
        let out = highlight(
            "no hits here",
            &["banana".into()],
            None,
            &HighlightOptions::default(),
        )
        .unwrap();
        assert_eq!(out, "no hits here");
    }

    #[test]
    fn fragment_view_emits_ellipsis_around_match() {
        let text = "abcdefghij ".repeat(40); // 440 chars, no matches
        let mut text = text;
        text.push_str("the quick brown fox jumps over a thing ");
        text.push_str(&"abcdefghij ".repeat(40));

        let opts = HighlightOptions {
            max_fragments: 1,
            fragment_size: 60,
            ..Default::default()
        };
        let out = highlight(&text, &["fox".into()], None, &opts).unwrap();
        assert!(out.contains("<b>fox</b>"));
        assert!(out.starts_with("..."));
        assert!(out.ends_with("..."));
    }

    #[test]
    fn fragment_view_emits_ellipsis_when_no_match_found() {
        let text = "a".repeat(500);
        let opts = HighlightOptions {
            max_fragments: 1,
            fragment_size: 30,
            ..Default::default()
        };
        let out = highlight(&text, &["zzz".into()], None, &opts).unwrap();
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().take_while(|c| *c == 'a').count(), 30);
    }

    #[test]
    fn analyzer_pipeline_matches_stemmed_form() {
        // Standard analyzer lower-cases and stems through Porter.
        let an = crate::analyzer::standard_analyzer("english");
        let out = highlight(
            "running quickly",
            &["runs".into()],
            Some(&an),
            &HighlightOptions::default(),
        )
        .unwrap();
        assert!(out.contains("<b>running</b>"), "got: {out}");
    }

    #[test]
    fn cjk_character_offsets_round_trip() {
        // A multi-byte text with the matched token in the middle.
        let text = "안녕 hello 세계";
        let out = highlight(text, &["hello".into()], None, &HighlightOptions::default()).unwrap();
        assert_eq!(out, "안녕 <b>hello</b> 세계");
    }

    #[test]
    fn analyzer_failure_is_returned_to_highlight_caller() {
        let analyzer = Analyzer::new(
            crate::Tokenizer::Pattern {
                pattern: "[".into(),
            },
            Vec::new(),
            Vec::new(),
        );
        let error = highlight(
            "searchable text",
            &["searchable".into()],
            Some(&analyzer),
            &HighlightOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            crate::AnalysisError::InvalidRegex {
                component: "pattern tokenizer",
                ..
            }
        ));
    }
}
