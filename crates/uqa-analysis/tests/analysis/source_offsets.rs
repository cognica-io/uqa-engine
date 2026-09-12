//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ops::Range;

use uqa_analysis::{FilteredText, SourceOffsets, TextCoordinates};

use super::*;

fn offsets(utf8: Range<usize>, utf16: Range<usize>) -> SourceOffsets {
    SourceOffsets { utf8, utf16 }
}

fn mapping(entries: &[(&str, &str)]) -> CharFilter {
    CharFilter::Mapping {
        mapping: entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
    }
}

#[test]
fn unicode_coordinates_distinguish_bytes_scalars_and_surrogate_pairs() {
    let coordinates = TextCoordinates::new("A한🙂e\u{301}");
    for (utf8, utf16) in [(0, 0), (1, 1), (4, 2), (8, 4), (9, 5), (11, 6)] {
        assert_eq!(coordinates.utf8_to_utf16(utf8).unwrap(), utf16);
        assert_eq!(coordinates.utf16_to_utf8(utf16).unwrap(), utf8);
    }
    assert_eq!(coordinates.offsets(1..8).unwrap(), offsets(1..8, 1..4));
    assert!(matches!(
        coordinates.utf8_to_utf16(2),
        Err(AnalysisError::InvalidTextOffset {
            coordinate: "UTF-8",
            ..
        })
    ));
    assert!(matches!(
        coordinates.utf16_to_utf8(3),
        Err(AnalysisError::InvalidTextOffset {
            coordinate: "UTF-16",
            ..
        })
    ));
    assert!(coordinates.utf8_to_utf16(12).is_err());
    assert!(coordinates.utf16_to_utf8(7).is_err());
    assert!(matches!(
        coordinates.offsets(Range { start: 4, end: 1 }),
        Err(AnalysisError::InvalidTextSpan { .. })
    ));
}

#[test]
fn html_tokens_and_entities_retain_original_ranges() {
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets("<b>한&amp;🙂</b>")
        .unwrap();
    assert_eq!(filtered.as_str(), " 한&🙂 ");
    assert_eq!(filtered.source_offsets(1..4).unwrap(), offsets(3..6, 3..4));
    assert_eq!(filtered.source_offsets(4..5).unwrap(), offsets(6..11, 4..9));
    assert_eq!(
        filtered.source_offsets(5..9).unwrap(),
        offsets(11..15, 9..11)
    );
    assert_eq!(
        filtered.source_offsets(1..9).unwrap(),
        offsets(3..15, 3..11)
    );
    assert_eq!(filtered.final_offsets(), offsets(19..19, 15..15));
    assert!(filtered.source_offsets(2..4).is_err());
    assert!(filtered.source_offsets_utf16(4..5).is_err());
}

#[test]
fn sequential_rules_and_filters_compose_without_losing_the_original() {
    let original = "여의도🙂";
    let first = mapping(&[("여의도", "서울")])
        .filter_with_offsets(original)
        .unwrap();
    assert_eq!(
        first.source_offsets_utf16(2..4).unwrap(),
        offsets(9..13, 3..5)
    );
    let second = mapping(&[("서울", "seoul")]).filter_mapped(first).unwrap();
    assert_eq!(second.as_str(), "seoul🙂");
    assert_eq!(second.original(), original);
    assert_eq!(second.source_offsets(2..4).unwrap(), offsets(0..9, 0..3));
    assert_eq!(
        second.source_offsets_utf16(5..7).unwrap(),
        offsets(9..13, 3..5)
    );
    assert!(second.source_offsets_utf16(5..6).is_err());

    let same_stage = mapping(&[("여의도", "서울"), ("서울", "seoul")])
        .filter_with_offsets(original)
        .unwrap();
    assert_eq!(same_stage.as_str(), "seoul🙂");
    assert_eq!(
        same_stage.source_offsets(0..5).unwrap(),
        offsets(0..9, 0..3)
    );
}

#[test]
fn deleted_interiors_and_trailing_text_have_distinct_source_boundaries() {
    let filtered = CharFilter::PatternReplace {
        pattern: "XX|YY".into(),
        replacement: String::new(),
    }
    .filter_with_offsets("abXXcdYY")
    .unwrap();
    assert_eq!(filtered.as_str(), "abcd");
    assert_eq!(filtered.source_offsets(2..4).unwrap(), offsets(4..6, 4..6));
    assert_eq!(filtered.source_offsets(1..3).unwrap(), offsets(1..5, 1..5));
    assert_eq!(filtered.source_offsets(2..2).unwrap(), offsets(4..4, 4..4));
    assert_eq!(filtered.source_offsets(4..4).unwrap(), offsets(8..8, 8..8));
}

#[test]
fn inserted_text_anchors_at_the_original_scalar_boundary() {
    let filtered = mapping(&[("", ".")]).filter_with_offsets("한🙂").unwrap();
    assert_eq!(filtered.as_str(), ".한.🙂.");
    assert_eq!(filtered.source_offsets(0..1).unwrap(), offsets(0..0, 0..0));
    assert_eq!(filtered.source_offsets(4..5).unwrap(), offsets(3..3, 1..1));
    assert_eq!(filtered.source_offsets(9..10).unwrap(), offsets(7..7, 3..3));
    assert_eq!(filtered.source_offsets(1..4).unwrap(), offsets(0..3, 0..1));
}

#[test]
fn reordered_regex_captures_cover_the_replaced_source() {
    let filtered = CharFilter::PatternReplace {
        pattern: "(서울)(역)".into(),
        replacement: "$2/$1".into(),
    }
    .filter_with_offsets("서울역")
    .unwrap();
    assert_eq!(filtered.as_str(), "역/서울");
    assert_eq!(filtered.source_offsets(0..3).unwrap(), offsets(0..9, 0..3));
    assert_eq!(filtered.source_offsets(4..10).unwrap(), offsets(0..9, 0..3));
}

#[test]
fn identity_replacements_preserve_exact_substring_offsets() {
    let filtered = CharFilter::PatternReplace {
        pattern: "(한글)".into(),
        replacement: "$0".into(),
    }
    .filter_with_offsets("한글")
    .unwrap();
    assert_eq!(filtered.source_offsets(3..6).unwrap(), offsets(3..6, 1..2));
}

#[test]
fn composed_html_entities_cover_every_consumed_entity() {
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets("&amp;lt;서울")
        .unwrap();
    assert_eq!(filtered.as_str(), "<서울");
    assert_eq!(filtered.source_offsets(0..1).unwrap(), offsets(0..8, 0..8));
    assert_eq!(
        filtered.source_offsets(1..7).unwrap(),
        offsets(8..14, 8..10)
    );
}

#[test]
fn removing_every_character_retains_the_final_original_offset() {
    let filtered = CharFilter::PatternReplace {
        pattern: ".+".into(),
        replacement: String::new(),
    }
    .filter_with_offsets("한🙂")
    .unwrap();
    assert!(filtered.as_str().is_empty());
    assert_eq!(filtered.source_offsets(0..0).unwrap(), offsets(7..7, 3..3));
    assert_eq!(filtered.final_offsets(), offsets(7..7, 3..3));
    assert_eq!(FilteredText::new("").final_offsets(), offsets(0..0, 0..0));
}

#[test]
fn zero_width_regex_replacements_preserve_unicode_text() {
    let filtered = CharFilter::PatternReplace {
        pattern: String::new(),
        replacement: "_".into(),
    }
    .filter_with_offsets("a한")
    .unwrap();
    assert_eq!(filtered.as_str(), "_a_한_");
    assert_eq!(filtered.source_offsets(3..6).unwrap(), offsets(1..4, 1..2));
    assert_eq!(filtered.source_offsets(6..7).unwrap(), offsets(4..4, 2..2));
}
