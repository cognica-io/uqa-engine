//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent allocating reference retained while verifying native highlighting ownership.

#![allow(
    clippy::similar_names,
    clippy::explicit_counter_loop,
    clippy::needless_range_loop,
    clippy::stable_sort_primitive,
    clippy::manual_midpoint,
    clippy::map_unwrap_or
)]

use super::super::HighlightOptions;
use crate::{AnalysisError, AnalysisResult, Analyzer, CompiledAnalyzer, TokenTerm};
use regex::Regex;
use std::collections::{BTreeSet, HashSet};

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

/// Highlight complete-source token spans with an explicit analyzer, or lower-case word matches when omitted.
///
/// Each query input is analyzed as a whole. An explicit analyzer is compiled once per call so source and query inputs share the same resolved revision.
pub fn highlight(
    text: &str,
    query_terms: &[String],
    analyzer: Option<&Analyzer>,
    opts: &HighlightOptions,
) -> AnalysisResult<String> {
    if text.is_empty() || query_terms.is_empty() {
        return Ok(text.to_owned());
    }
    match analyzer {
        Some(analyzer) => {
            let compiled = analyzer.compile()?;
            highlight_compiled(text, query_terms, &compiled, opts)
        }
        None => highlight_words(text, query_terms, None, opts),
    }
}

/// Highlight independently analyzed regex words, retaining the word-scanning contract used by SQL calls without an explicit analyzer name.
pub fn highlight_words(
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

    Ok(render_highlights(text, &match_spans, opts))
}

pub(super) fn render_highlights(
    text: &str,
    match_spans: &[(usize, usize)],
    opts: &HighlightOptions,
) -> String {
    if match_spans.is_empty() {
        return if opts.max_fragments > 0 {
            ellipsis_prefix(text, opts.fragment_size)
        } else {
            text.to_owned()
        };
    }
    if opts.max_fragments > 0 {
        build_fragments(text, match_spans, opts)
    } else {
        wrap_full(text, match_spans, &opts.start_tag, &opts.end_tag)
    }
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
        let focus = cluster
            .iter()
            .min_by_key(|(start, end)| (start + (end - start) / 2).abs_diff(centre))
            .expect("nonempty highlight cluster");
        let mut frag_start = centre.saturating_sub(half).min(focus.0);
        let mut frag_end = centre.saturating_add(half).min(total_chars).max(focus.1);

        // Snap to nearest space boundary so we do not bisect a word.
        if frag_start > 0 {
            let mut probe = frag_start;
            let limit = frag_start.saturating_add(30).min(focus.0);
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
            let lower = frag_end.saturating_sub(30).max(focus.1);
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

pub fn highlight_compiled(
    text: &str,
    query_inputs: &[String],
    analyzer: &CompiledAnalyzer,
    opts: &HighlightOptions,
) -> AnalysisResult<String> {
    if text.is_empty() || query_inputs.is_empty() {
        return Ok(text.to_owned());
    }
    let mut terms = HashSet::<TokenTerm>::new();
    for query in query_inputs {
        terms.extend(
            analyzer
                .analyze_tokens(query)?
                .into_tokens()
                .into_iter()
                .map(|token| token.term),
        );
    }
    if terms.is_empty() {
        return Ok(text.to_owned());
    }
    let source = analyzer.analyze_tokens(text)?;
    let mut spans = Vec::new();
    for token in source.tokens() {
        if !terms.contains(token.term()) {
            continue;
        }
        let range = &token
            .offsets()
            .ok_or(AnalysisError::MissingTokenOffsets)?
            .utf8;
        if range.start > range.end {
            return Err(AnalysisError::InvalidTextSpan {
                start: range.start,
                end: range.end,
            });
        }
        for offset in [range.start, range.end] {
            if !text.is_char_boundary(offset) {
                return Err(AnalysisError::InvalidTextOffset {
                    coordinate: "UTF-8",
                    offset,
                    length: text.len(),
                });
            }
        }
        if !range.is_empty() {
            spans.push((range.start, range.end));
        }
    }
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        if let Some(previous) = merged.last_mut().filter(|previous| start < previous.1) {
            previous.1 = previous.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    // Walk disjoint source ranges once to obtain the renderer's Unicode character coordinates.
    let (mut byte, mut character) = (0, 0);
    for span in &mut merged {
        character += text[byte..span.0].chars().count();
        let start = character;
        character += text[span.0..span.1].chars().count();
        byte = span.1;
        *span = (start, character);
    }
    Ok(render_highlights(text, &merged, opts))
}
