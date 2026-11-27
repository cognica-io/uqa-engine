//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::stream::replace_literal;
use super::*;
use crate::CharFilter;

fn compare_ranges(old: &FilteredText<'_>, new: &FilteredText<'_>) {
    assert_eq!(old.as_str(), new.as_str());
    assert_eq!(old.final_offsets(), new.final_offsets());
    let boundaries: Vec<_> = old
        .as_str()
        .char_indices()
        .map(|(i, _)| i)
        .chain([old.as_str().len()])
        .collect();
    for (i, &start) in boundaries.iter().enumerate() {
        for &end in &boundaries[i..] {
            assert_eq!(
                old.source_offsets(start..end).unwrap(),
                new.source_offsets(start..end).unwrap()
            );
        }
    }
    let end = old.as_str().encode_utf16().count();
    for i in 0..=end {
        for j in i..=end {
            assert_eq!(
                old.source_offsets_utf16(i..j)
                    .map_err(|error| error.to_string()),
                new.source_offsets_utf16(i..j)
                    .map_err(|error| error.to_string())
            );
            assert_eq!(
                old.source_covering_offsets_utf16(i..j).unwrap(),
                new.source_covering_offsets_utf16(i..j).unwrap()
            );
        }
    }
}

#[test]
fn compound_and_nested_html_entities_preserve_every_original_span() {
    for text in [
        "&amp;lt;&amp;gt;&amp;quot;&amp;#39;&amp;apos;&amp;nbsp;",
        "🙂&amp;amp;lt;&A&amp;&lt;&amp; &amp;&amp;lt;",
        "<b>&amp;lt;</b><>&amp;xx &qu",
    ] {
        let budget = MemoryBudget::new(1 << 20);
        let mut original = FilteredText::new(text);
        super::super::stream::replace_html(&mut original, &budget, &mut || Ok(())).unwrap();
        for &(entity, value) in HTML_ENTITIES {
            replace_literal(&mut original, entity, value, &budget, &mut || Ok(())).unwrap();
        }
        original
            .prepare_coordinates(&budget, &mut || Ok(()))
            .unwrap();
        let actual = CharFilter::HTMLStrip.filter_with_offsets(text).unwrap();
        compare_ranges(&original, &actual);
    }
}

#[test]
fn independent_scalar_edits_preserve_ordered_cascades_and_all_offsets() {
    for rules in [
        vec![("a", "b"), ("b", "🙂"), ("z", ""), ("é", "ä")],
        vec![("ab", "é"), ("ba", "X"), ("a", "aa"), ("b", "c"), ("c", "")],
        vec![("a", ""), ("x", "X"), ("b", ""), ("", "-")],
        vec![("🙂", "é"), ("é", "🙂"), ("b", "é"), ("z", ""), ("a", "bb")],
    ] {
        let mapping = rules
            .into_iter()
            .map(|(s, t)| (s.to_owned(), t.to_owned()))
            .collect::<BTreeMap<_, _>>();
        let ordered = crate::work::inline(mapping_longest_first(&mapping, &mut ())).unwrap();
        let filter = CharFilter::Mapping { mapping };
        for text in ["ababa", "z🙂bé", "🙂🙂 xé", "b", ""] {
            let budget = MemoryBudget::new(1 << 20);
            let mut original = FilteredText::new(text);
            for (key, value) in &ordered {
                replace_literal(&mut original, key, value, &budget, &mut || Ok(())).unwrap();
            }
            original
                .prepare_coordinates(&budget, &mut || Ok(()))
                .unwrap();
            let actual = filter.filter_with_offsets(text).unwrap();
            compare_ranges(&original, &actual);
        }
    }
}

#[test]
fn prepared_rules_retain_overlapping_priority_and_spans_across_generated_inputs() {
    let keys = [
        "a", "b", "é", "🙂", "ab", "ba", "aa", "abé", "éa", "b🙂", "",
    ];
    let values = ["", "a", "b", "ab", "é", "🙂", "ba", "xx"];
    let alphabet = ["a", "b", "é", "🙂"];
    let budget = MemoryBudget::new(1 << 20);
    let mut seed = 73_u32;
    let mut draw = |len: usize| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((seed >> 8) as usize) % len
    };
    for _ in 0..256 {
        let mut mapping = BTreeMap::new();
        for _ in 0..5 {
            mapping.insert(
                keys[draw(keys.len())].to_owned(),
                values[draw(values.len())].to_owned(),
            );
        }
        let ordered = crate::work::inline(mapping_longest_first(&mapping, &mut ())).unwrap();
        let mappings = crate::work::inline(LiteralMappings::prepare(&mapping, &mut ())).unwrap();
        for _ in 0..8 {
            let input: String = (0..6).map(|_| alphabet[draw(alphabet.len())]).collect();
            let mut reference = FilteredText::new(&input);
            let mut actual = FilteredText::new(&input);
            for (key, value) in &ordered {
                replace_literal(&mut reference, key, value, &budget, &mut || Ok(())).unwrap();
            }
            mappings
                .replace(&mut actual, &budget, &mut || Ok(()))
                .unwrap();
            reference
                .prepare_coordinates(&budget, &mut || Ok(()))
                .unwrap();
            actual.prepare_coordinates(&budget, &mut || Ok(())).unwrap();
            assert_eq!(
                actual.as_str(),
                reference.as_str(),
                "{mapping:?}, {input:?}"
            );
            compare_ranges(&reference, &actual);
        }
    }
}

#[test]
fn independent_compound_rules_scan_unmatched_input_once_with_no_allocation() {
    let mapping: BTreeMap<_, _> = (0..512)
        .map(|index| (format!("{{{index:03x}}}"), "#".to_owned()))
        .collect();
    let mappings = crate::work::inline(LiteralMappings::prepare(&mapping, &mut ())).unwrap();
    let input = "x".repeat(32 * 1024);
    let mut text = FilteredText::new(&input);
    let budget = MemoryBudget::new(0);
    let mut polls = 0;
    let cost = allocation_counter::measure(|| {
        mappings
            .replace(&mut text, &budget, &mut || {
                polls += 1;
                Ok(())
            })
            .unwrap();
    });
    assert!((32..=40).contains(&polls), "{polls} polls");
    assert_eq!(cost.count_total, 0, "{cost:?}");
    assert_eq!(text.as_str().as_ptr(), input.as_ptr());
    assert_eq!(budget.peak(), 0);
    let mut interrupted = 0;
    let result = mappings.replace(&mut text, &budget, &mut || {
        interrupted += 1;
        if interrupted == 8 {
            Err(AnalysisError::Cancelled)
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(AnalysisError::Cancelled)));
    assert_eq!(text.as_str().as_ptr(), input.as_ptr());
}
