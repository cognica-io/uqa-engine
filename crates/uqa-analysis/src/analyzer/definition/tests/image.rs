//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{CharFilter, NormalizationConfig, TokenFilter, Tokenizer};
use uqa_core::memory::{Budgeted, BudgetedVec};

fn admitted(bytes: &[u8], memory: &MemoryBudget) -> Budgeted<Vec<u8>> {
    let mut value = BudgetedVec::new(memory);
    value.extend_from_slice(bytes).unwrap();
    let (value, memory) = value.into_parts();
    Budgeted::new(value, memory)
}

fn restored(config: &Analyzer) {
    let bytes = serde_json::to_vec(config).unwrap();
    let memory = MemoryBudget::new(8 << 20);
    let input = admitted(&bytes, &memory);
    let original = input.as_ptr();
    let token = CancellationToken::new();
    let value = AnalyzerDefinition::from_image(input, &token).unwrap();
    assert_eq!(serde_json::to_vec(value.configuration()).unwrap(), bytes);
    assert_eq!(value.image.as_ref().unwrap().as_bytes(), bytes);
    assert_eq!(value.image.as_ref().unwrap().as_ptr(), original);
    assert!(memory.used() > bytes.len());
    assert!(memory.peak() <= memory.limit());
    drop(value);
    assert_eq!(memory.used(), 0);
}

#[test]
fn original_analyzer_input_preserves_every_common_stage_and_native_setting() {
    let filters = vec![
        TokenFilter::Lowercase,
        TokenFilter::ASCIIFolding,
        TokenFilter::PorterStem,
        TokenFilter::Stop {
            language: "not-a-built-in".into(),
            custom_words: vec![String::new(), "한🙂\"\\\n".into(), "x".into(), "x".into()],
        },
        TokenFilter::Synonym {
            synonyms: std::collections::BTreeMap::new(),
            synonyms_path: None,
        },
        TokenFilter::Synonym {
            synonyms: [
                ("a".into(), vec!["b".into(), "c".into()]),
                ("z".into(), vec![]),
            ]
            .into(),
            synonyms_path: None,
        },
        TokenFilter::Synonym {
            synonyms: std::collections::BTreeMap::new(),
            synonyms_path: Some("unavailable-during-import/synonyms.txt".into()),
        },
        TokenFilter::Synonym {
            synonyms: [("x".into(), vec!["y".into()])].into(),
            synonyms_path: Some(std::path::PathBuf::new()),
        },
        TokenFilter::Ngram {
            min_gram: 0,
            max_gram: usize::MAX,
            keep_short: true,
        },
        TokenFilter::EdgeNgram {
            min_gram: 9,
            max_gram: 0,
        },
        TokenFilter::Length {
            min_length: usize::MAX,
            max_length: 0,
        },
    ];
    let chars = vec![
        CharFilter::HTMLStrip,
        CharFilter::CJKWidth,
        CharFilter::PatternReplace {
            pattern: "[".into(),
            replacement: "$1\n🙂".into(),
        },
        CharFilter::Mapping {
            mapping: [(String::new(), "x".into()), ("한".into(), "🙂".into())].into(),
        },
    ];
    for tokenizer in [
        Tokenizer::Whitespace,
        Tokenizer::Standard,
        Tokenizer::Letter,
        Tokenizer::Keyword,
        Tokenizer::Pattern {
            pattern: "[".into(),
        },
        Tokenizer::NGram {
            min_gram: 0,
            max_gram: usize::MAX,
        },
    ] {
        for normalization in [
            None,
            Some(NormalizationConfig::Unavailable),
            Some(NormalizationConfig::CJKWidth),
        ] {
            restored(&Analyzer {
                tokenizer: tokenizer.clone(),
                token_filters: filters.clone(),
                char_filters: chars.clone(),
                normalization,
            });
        }
    }
    restored(&Analyzer::default());
}

#[test]
fn original_analyzer_input_preserves_literal_wire_and_defers_resource_access() {
    let expected = br#"{"tokenizer":{"type":"pattern","pattern":"["},"token_filters":[{"type":"synonym","synonyms_path":"unavailable-during-import/synonyms.txt"}],"char_filters":[]}"#;
    let memory = MemoryBudget::new(16 << 10);
    let token = CancellationToken::new();
    let value = AnalyzerDefinition::from_image(admitted(expected, &memory), &token).unwrap();
    assert_eq!(serde_json::to_vec(&value).unwrap(), expected);
    assert!(matches!(
        value.configuration().clone().compile(),
        Err(AnalysisError::InvalidRegex { .. })
    ));
    drop(value);
    assert_eq!(memory.used(), 0);
    let expected = br#"{"tokenizer":{"type":"keyword"},"token_filters":[{"type":"synonym","synonyms_path":"unavailable-during-import/synonyms.txt"}],"char_filters":[]}"#;
    let value = AnalyzerDefinition::from_image(admitted(expected, &memory), &token).unwrap();
    assert_eq!(serde_json::to_vec(&value).unwrap(), expected);
    assert!(matches!(
        value.configuration().clone().compile(),
        Err(AnalysisError::SynonymFile(_))
    ));
}

#[test]
fn original_analyzer_input_rejects_partial_and_ambiguous_native_shapes() {
    let valid = br#"{"tokenizer":{"type":"pattern","pattern":"a"},"token_filters":[{"type":"synonym","synonyms":{"a":["b"],"z":[]}}],"char_filters":[]}"#;
    let token = CancellationToken::new();
    for end in 0..valid.len() {
        let memory = MemoryBudget::new(32 << 10);
        assert!(
            AnalyzerDefinition::from_image(admitted(&valid[..end], &memory), &token).is_err(),
            "prefix {end}"
        );
        assert_eq!(memory.used(), 0, "prefix {end}");
    }
    for bytes in [
        br#"{"tokenizer":{"type":"other"},"token_filters":[],"char_filters":[]}"#.as_slice(),
        br#"{"tokenizer":{"type":"keyword","extra":0},"token_filters":[],"char_filters":[]}"#,
        br#"{"tokenizer":{"type":"keyword"},"tokenizer":{"type":"keyword"},"token_filters":[],"char_filters":[]}"#,
        br#"{"tokenizer":{"type":"keyword"},"token_filters":[{"type":"synonym","synonyms":{"x":[],"x":[]}}],"char_filters":[]}"#,
        br#"{"tokenizer":{"type":"keyword"},"token_filters":[{"type":"synonym","synonyms":{"z":[],"a":[]}}],"char_filters":[]}"#,
        br#"{"tokenizer":{"type":"n_gram","min_gram":18446744073709551616,"max_gram":2},"token_filters":[],"char_filters":[]}"#,
        br#"{"tokenizer":{"type":"keyword"},"token_filters":[],"char_filters":[],"normalization":null}"#,
        br#"{"tokenizer":{"type":"keyword"},"token_filters":[],"char_filters":[]}0"#,
    ] {
        let memory = MemoryBudget::new(32 << 10);
        assert!(AnalyzerDefinition::from_image(admitted(bytes, &memory), &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn original_analyzer_input_retains_admission_through_independent_consumers() {
    let source = Analyzer {
        tokenizer: Tokenizer::Pattern {
            pattern: "x".repeat(1 << 20),
        },
        ..Analyzer::default()
    };
    let bytes = serde_json::to_vec(&source).unwrap();
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let value = AnalyzerDefinition::from_image(admitted(&bytes, &memory), &token).unwrap();
    let configuration = Arc::downgrade(&value.configuration);
    let before = memory.used();
    let retained = value.clone();
    let capture = MemoryBudget::new(8 << 20);
    let image = value
        .capture_image(&ProductionControl::new(&capture, &token, &token))
        .unwrap();
    drop(value);
    assert_eq!(memory.used(), before);
    std::thread::spawn(move || drop(retained)).join().unwrap();
    assert!(configuration.upgrade().is_none());
    assert!(memory.used() > 0 && memory.used() < before);
    assert_eq!(image.as_bytes(), bytes);
    drop(image);
    assert_eq!(memory.used(), 0);
    assert_eq!(capture.used(), 0);
}

#[test]
fn original_analyzer_input_releases_cancelled_and_underfunded_construction() {
    let source = Analyzer {
        token_filters: vec![TokenFilter::Stop {
            language: "english".into(),
            custom_words: (0..128)
                .map(|i| format!("{i:04}{}", "x".repeat(256)))
                .collect(),
        }],
        ..Analyzer::default()
    };
    let bytes = serde_json::to_vec(&source).unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let memory = MemoryBudget::new(1 << 20);
    assert!(matches!(
        AnalyzerDefinition::from_image(admitted(&bytes, &memory), &cancelled),
        Err(AnalyzerConfigurationError::Admission(
            ValueRetentionError::Cancelled(_)
        ))
    ));
    assert_eq!(memory.used(), 0);
    for extra in [0, 32, 128, 512, 4096, 16384] {
        let memory = MemoryBudget::new(bytes.len() + extra);
        let input = admitted(&bytes, &memory);
        let token = CancellationToken::new();
        assert!(matches!(
            AnalyzerDefinition::from_image(input, &token),
            Err(
                AnalyzerConfigurationError::Decoding(JsonReadError::Memory(_))
                    | AnalyzerConfigurationError::Admission(ValueRetentionError::Memory(_))
            )
        ));
        assert_eq!(memory.used(), 0, "extra {extra}");
        assert!(memory.peak() <= memory.limit());
    }
    restored(&source);
}

mod languages;
