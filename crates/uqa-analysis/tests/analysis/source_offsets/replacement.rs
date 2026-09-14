//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use regex::Regex;
use uqa_analysis::CharFilter;
use uqa_core::memory::MemoryBudget;

#[test]
fn streamed_replacements_match_regex_capture_and_empty_match_rules() {
    let patterns = [
        "",
        "a*",
        "(?P<x>韓|a)(?P<y>🙂)?",
        "(?P<x>a)?",
        "(?m)^|$",
        "(?P<한>韓|a)",
        r"\b(韓|a+)\b",
        r"\B(a*)",
        "((a?)*)",
        "(^ab|b|x)",
    ];
    let inputs = [
        "",
        "a",
        "aa",
        "aba",
        "xab",
        "韓🙂a",
        "🙂\n韓",
        "$x{}",
        "b🙂c",
    ];
    let replacements = [
        "$",
        "$$",
        "$$$x",
        "$0",
        "${0}",
        "$1",
        "${1}",
        "$1x",
        "${1}x",
        "$x",
        "${x}",
        "${x}🙂$y",
        "$missing",
        "${missing}",
        "${}",
        "${",
        "${x",
        "${x}${x}",
        "${x}{",
        "${x}$$",
        "$99999999999999999999999999999999",
        "\0",
        "🙂",
        "$_",
        "${bad-name}",
        "$é",
        "$x_",
        "${한}",
    ];
    for pattern in patterns {
        let oracle = Regex::new(pattern).unwrap();
        for replacement in replacements {
            let filter = CharFilter::PatternReplace {
                pattern: pattern.to_owned(),
                replacement: replacement.to_owned(),
            };
            for input in inputs {
                let budget = MemoryBudget::new(1 << 20);
                let expected = oracle.replace_all(input, replacement);
                let actual = filter
                    .filter_with_offsets_budgeted(input, &budget, &mut || Ok(()))
                    .unwrap();
                assert_eq!(
                    actual.as_str(),
                    expected,
                    "pattern={pattern:?}, input={input:?}, replacement={replacement:?}"
                );
                assert_eq!(actual.final_offsets().utf8.end, input.len());
                assert_eq!(
                    actual.final_offsets().utf16.end,
                    input.encode_utf16().count()
                );
                drop(actual);
                assert_eq!(budget.used(), 0);
            }
        }
    }
}

#[test]
fn capture_reordering_covers_the_whole_match_and_identity_keeps_exact_spans() {
    let budget = MemoryBudget::new(4096);
    let reordered = CharFilter::PatternReplace {
        pattern: "(ab)-(cd)".to_owned(),
        replacement: "$2/$1".to_owned(),
    }
    .filter_with_offsets_budgeted("앞 ab-cd 🙂", &budget, &mut || Ok(()))
    .unwrap();
    assert_eq!(reordered.as_str(), "앞 cd/ab 🙂");
    for range in [4..5, 6..7, 7..9] {
        assert_eq!(reordered.source_offsets(range).unwrap().utf8, 4..9);
    }
    let identity = CharFilter::PatternReplace {
        pattern: "(ab)-(cd)".to_owned(),
        replacement: "$1-$2".to_owned(),
    }
    .filter_with_offsets_budgeted("앞 ab-cd 🙂", &budget, &mut || Ok(()))
    .unwrap();
    assert_eq!(identity.source_offsets(4..5).unwrap().utf8, 4..5);
    assert_eq!(identity.source_offsets(7..9).unwrap().utf8, 7..9);
}
