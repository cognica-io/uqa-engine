//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

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

mod control;
mod reference;
mod rendering;
