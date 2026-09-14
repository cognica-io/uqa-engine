//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ops::Range;

use uqa_analysis::{FilteredText, SourceOffsets, TextCoordinates};

use super::*;

#[path = "source_offsets/budget.rs"]
mod budget;
#[path = "source_offsets/replacement.rs"]
mod replacement;

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

#[test]
fn covering_coordinates_retain_every_utf16_range_without_splitting_utf8() {
    let text = "A한🙂e\u{301}";
    let coordinates = TextCoordinates::new(text);
    let lower = [0, 1, 4, 4, 8, 9, 11];
    let upper = [0, 1, 4, 8, 8, 9, 11];
    for (start, &lower) in lower.iter().enumerate() {
        for (end, &upper) in upper.iter().enumerate().skip(start) {
            let source = coordinates.covering_offsets_utf16(start..end).unwrap();
            assert_eq!(source, offsets(lower..upper, start..end));
            assert!(text.get(source.utf8).is_some());
        }
    }
    for text in ["", "ascii", "한", "🙂"] {
        let coordinates = TextCoordinates::new(text);
        let length = coordinates.utf16_len();
        assert_eq!(
            coordinates.covering_offsets_utf16(0..length).unwrap(),
            offsets(0..text.len(), 0..length)
        );
        assert!(matches!(
            coordinates.covering_offsets_utf16(0..length + 1),
            Err(AnalysisError::InvalidTextOffset { .. })
        ));
        assert!(matches!(
            coordinates.covering_offsets_utf16(Range { start: 1, end: 0 }),
            Err(AnalysisError::InvalidTextSpan { .. })
        ));
    }
}

#[test]
fn split_pairs_survive_composed_replacement_deletion_and_insertion_maps() {
    let original = "한<b>🙂a&amp;𐐀</b>끝";
    let html = CharFilter::HTMLStrip.filter_with_offsets(original).unwrap();
    let filtered = mapping(&[("한 ", ""), ("a&", "XY"), ("𐐀", "𐐨")])
        .filter_mapped(html)
        .unwrap();
    assert_eq!(filtered.as_str(), "🙂XY𐐨 끝");
    for (span, expected) in [
        (0..1, offsets(6..10, 4..5)),
        (1..2, offsets(6..10, 5..6)),
        (1..1, offsets(6..10, 5..5)),
        (4..5, offsets(16..20, 12..14)),
        (1..5, offsets(6..20, 5..14)),
    ] {
        assert_eq!(
            filtered.source_covering_offsets_utf16(span).unwrap(),
            expected
        );
    }
    // The unchanged strict API still rejects the same split pair.
    assert!(filtered.source_offsets_utf16(1..2).is_err());
    let inserted = CharFilter::PatternReplace {
        pattern: "^".into(),
        replacement: "🙂".into(),
    }
    .filter_mapped(filtered)
    .unwrap();
    assert_eq!(
        inserted.source_covering_offsets_utf16(0..1).unwrap(),
        offsets(6..6, 4..4)
    );
    let deleted = mapping(&[(" 끝", "")]).filter_mapped(inserted).unwrap();
    let end = deleted.as_str().encode_utf16().count();
    assert_eq!(
        deleted.source_covering_offsets_utf16(end..end).unwrap(),
        deleted.final_offsets()
    );
    assert_eq!(
        deleted.final_offsets(),
        offsets(original.len()..original.len(), 19..19)
    );
    assert!(deleted.source_covering_offsets_utf16(end..end + 1).is_err());
}

#[test]
fn covering_and_strict_projection_agree_at_all_scalar_boundaries() {
    for original in ["", "ascii", "<b>🙂a</b>", "&amp;lt;한🙂", "𐐀한🙂끝"] {
        let html = CharFilter::HTMLStrip.filter_with_offsets(original).unwrap();
        let mapped = mapping(&[("한", "XY🙂"), ("a", ""), ("끝", "")])
            .filter_mapped(html)
            .unwrap();
        let filtered = CharFilter::PatternReplace {
            pattern: "^|$".into(),
            replacement: "𐐨".into(),
        }
        .filter_mapped(mapped)
        .unwrap();
        let coordinates = TextCoordinates::new(filtered.as_str());
        let boundaries: Vec<_> = (0..=coordinates.utf16_len())
            .filter(|offset| coordinates.utf16_to_utf8(*offset).is_ok())
            .collect();
        for (index, &start) in boundaries.iter().enumerate() {
            for &end in &boundaries[index..] {
                assert_eq!(
                    filtered.source_covering_offsets_utf16(start..end).unwrap(),
                    filtered.source_offsets_utf16(start..end).unwrap(),
                    "{original:?} {start}..{end}"
                );
            }
        }
    }
}

#[cfg(feature = "nori")]
#[test]
fn accepted_nori_user_offsets_keep_exact_units_and_safe_original_spans() {
    use uqa_analysis::nori::{KoreanTokenizer, NoriOptions, UserDictionary, UserDictionaryLimits};
    let model = super::nori_resources::model().clone();
    let user =
        UserDictionary::compile("🙂a 가 나", &model, UserDictionaryLimits::default()).unwrap();
    let tokenizer = KoreanTokenizer::new(model, user, NoriOptions::default()).unwrap();
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets("<b>🙂a</b>")
        .unwrap();
    let output = tokenizer.tokenize(filtered.as_str()).unwrap();
    assert_eq!(output.tokens.len(), 2);
    assert_eq!(output.tokens[0].term_utf16, [0xd83d]);
    assert_eq!(output.tokens[1].term_utf16, [0xde42]);
    for (token, expected) in output
        .tokens
        .iter()
        .zip([offsets(3..7, 4..5), offsets(7..8, 5..6)])
    {
        assert_eq!(
            filtered
                .source_covering_offsets_utf16(token.start_utf16..token.end_utf16)
                .unwrap(),
            expected
        );
        assert!(filtered.original().get(expected.utf8).is_some());
    }
}
