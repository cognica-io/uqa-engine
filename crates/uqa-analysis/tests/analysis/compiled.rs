//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{collections::BTreeMap, sync::Arc};

use uqa_analysis::{AnalysisError, Analyzer, CharFilter, CompiledAnalyzer, TokenFilter, Tokenizer};

#[path = "compiled/memory.rs"]
mod memory;

#[test]
fn compiled_pipeline_owns_configuration_and_preserves_complete_graph_and_source_state() {
    let input = "<b>HÉLLO cats</b> and";
    let compiled = {
        let mut analyzer = Analyzer::new(
            Tokenizer::Pattern {
                pattern: "\\s+".into(),
            },
            vec![
                TokenFilter::Lowercase,
                TokenFilter::ASCIIFolding,
                TokenFilter::Stop {
                    language: "english".into(),
                    custom_words: vec!["ignored".into()],
                },
                TokenFilter::PorterStem,
                TokenFilter::Synonym {
                    synonyms: BTreeMap::from([(
                        "hello".into(),
                        vec!["hi".into(), "greeting".into()],
                    )]),
                    synonyms_path: None,
                },
            ],
            vec![CharFilter::HTMLStrip],
        );
        let configuration = serde_json::to_value(&analyzer).unwrap();
        let compiled = analyzer.compile().unwrap();
        assert_eq!(configuration, serde_json::to_value(&analyzer).unwrap());
        analyzer.tokenizer = Tokenizer::Keyword;
        analyzer.token_filters.clear();
        analyzer.char_filters.clear();
        assert_eq!(analyzer.analyze(input).unwrap(), [input]);
        compiled
    };
    let output = compiled.analyze_tokens(input).unwrap();
    assert_eq!(
        output.clone().into_terms().unwrap(),
        ["hello", "hi", "greeting", "cat"]
    );
    for (index, token) in output.tokens().iter().enumerate() {
        assert_eq!(
            token.position_increment(),
            u32::from(index == 0 || index == 3)
        );
        assert_eq!(token.position_length(), 1);
        assert!(!token.is_keyword());
        assert_eq!(
            token.offsets().unwrap().utf8,
            if index == 3 { 10..14 } else { 3..9 }
        );
        assert_eq!(
            token.offsets().unwrap().utf16,
            if index == 3 { 9..13 } else { 3..8 }
        );
    }
    assert_eq!(output.final_position_increment(), 1);
    assert_eq!(output.final_offsets().utf8, 22..22);
    assert_eq!(output.final_offsets().utf16, 21..21);
    assert_eq!(compiled.analyze_tokens(input).unwrap(), output);
}

#[test]
fn compiled_synonyms_remain_fixed_after_edits_deletion_and_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synonyms.txt");
    std::fs::write(&path, "cat => feline\n").unwrap();
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Synonym {
            synonyms: BTreeMap::from([("cat".into(), vec!["ignored".into()])]),
            synonyms_path: Some(path.clone()),
        }],
        Vec::new(),
    );
    let first = analyzer.compile().unwrap();
    std::fs::write(&path, "cat => animal\n").unwrap();
    let second = analyzer.compile().unwrap();
    assert_eq!(analyzer.analyze("cat").unwrap(), ["cat", "animal"]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(first.analyze("cat").unwrap(), ["cat", "feline"]);
    assert_eq!(second.analyze("cat").unwrap(), ["cat", "animal"]);
    assert!(matches!(
        analyzer.analyze("cat"),
        Err(AnalysisError::SynonymFile(_))
    ));
    assert!(matches!(
        analyzer.compile(),
        Err(AnalysisError::SynonymFile(_))
    ));
    std::fs::write(&path, "cat => third\n").unwrap();
    assert_eq!(
        analyzer.compile().unwrap().analyze("cat").unwrap(),
        ["cat", "third"]
    );
    assert_eq!(first.analyze("cat").unwrap(), ["cat", "feline"]);
    assert_eq!(second.analyze("cat").unwrap(), ["cat", "animal"]);
}

#[test]
fn compilation_rejects_invalid_stages_in_pipeline_order() {
    let directory = tempfile::tempdir().unwrap();
    let mut analyzer = Analyzer::new(
        Tokenizer::Pattern {
            pattern: "[".into(),
        },
        vec![TokenFilter::Synonym {
            synonyms: BTreeMap::new(),
            synonyms_path: Some(directory.path().join("missing")),
        }],
        vec![CharFilter::PatternReplace {
            pattern: "[".into(),
            replacement: String::new(),
        }],
    );
    assert!(matches!(
        analyzer.compile(),
        Err(AnalysisError::InvalidRegex {
            component: "pattern-replace character filter",
            ..
        })
    ));
    analyzer.char_filters.clear();
    assert!(matches!(
        analyzer.compile(),
        Err(AnalysisError::InvalidRegex {
            component: "pattern tokenizer",
            ..
        })
    ));
    analyzer.tokenizer = Tokenizer::NGram {
        min_gram: 0,
        max_gram: 2,
    };
    assert!(matches!(
        analyzer.compile(),
        Err(AnalysisError::InvalidGramBounds {
            component: "n-gram tokenizer",
            ..
        })
    ));
    analyzer.tokenizer = Tokenizer::Whitespace;
    assert!(matches!(
        analyzer.compile(),
        Err(AnalysisError::SynonymFile(_))
    ));
    for filter in [
        TokenFilter::Ngram {
            min_gram: 2,
            max_gram: 1,
            keep_short: true,
        },
        TokenFilter::EdgeNgram {
            min_gram: 0,
            max_gram: 1,
        },
    ] {
        analyzer.token_filters = vec![filter];
        assert!(matches!(
            analyzer.compile(),
            Err(AnalysisError::InvalidGramBounds { .. })
        ));
    }
    analyzer.token_filters = vec![TokenFilter::Lowercase];
    assert_eq!(analyzer.compile().unwrap().analyze("OK").unwrap(), ["ok"]);
}

#[test]
fn prepared_stage_parameters_preserve_existing_results_including_empty_streams() {
    let tokenizers = [
        Tokenizer::Whitespace,
        Tokenizer::Standard,
        Tokenizer::Letter,
        Tokenizer::NGram {
            min_gram: 1,
            max_gram: 3,
        },
        Tokenizer::Pattern {
            pattern: "(?:^|[,;])".into(),
        },
        Tokenizer::Keyword,
    ];
    let token_filters = [
        TokenFilter::Lowercase,
        TokenFilter::Stop {
            language: "english".into(),
            custom_words: vec!["中".into(), "🙂".into()],
        },
        TokenFilter::PorterStem,
        TokenFilter::ASCIIFolding,
        TokenFilter::Synonym {
            synonyms: BTreeMap::from([("x".into(), vec![String::new(), "x".into(), "x".into()])]),
            synonyms_path: None,
        },
        TokenFilter::Ngram {
            min_gram: 2,
            max_gram: 3,
            keep_short: true,
        },
        TokenFilter::EdgeNgram {
            min_gram: 2,
            max_gram: 4,
        },
        TokenFilter::Length {
            min_length: 2,
            max_length: 4,
        },
    ];
    let char_filters = [
        CharFilter::HTMLStrip,
        CharFilter::Mapping {
            mapping: BTreeMap::from([("AA".into(), "A".into()), ("A".into(), "中".into())]),
        },
        CharFilter::PatternReplace {
            pattern: "(x)(y)|$".into(),
            replacement: "$2$1🙂".into(),
        },
    ];
    for tokenizer in tokenizers {
        for filter in &token_filters {
            let analyzer = Analyzer::new(
                tokenizer.clone(),
                vec![filter.clone()],
                char_filters.to_vec(),
            );
            let compiled = analyzer.compile().unwrap();
            let restored =
                uqa_analysis::AnalyzerResources::new(uqa_analysis::AnalyzerLimits::default())
                    .restore_json(compiled.descriptor().canonical_json())
                    .unwrap();
            for input in [
                "",
                "and ",
                "x x",
                "<b>AA</b>,xy;the",
                "HÉLLO 中 🙂 𐐀",
                "\r\n\u{85}\u{a0}\u{2028}",
            ] {
                assert_eq!(
                    compiled.analyze_tokens(input).unwrap(),
                    analyzer.analyze_tokens(input).unwrap(),
                    "{analyzer:?}: {input:?}"
                );
                assert_eq!(
                    restored.analyze_tokens(input).unwrap(),
                    compiled.analyze_tokens(input).unwrap()
                );
            }
        }
    }
}

#[test]
fn cloned_compiled_handles_keep_concurrent_execution_state_separate() {
    fn is_send_sync<T: Send + Sync>() {}
    is_send_sync::<CompiledAnalyzer>();
    let analyzer = uqa_analysis::standard_analyzer("english");
    let compiled = analyzer.compile().unwrap();
    let expected: Vec<_> = [
        "",
        "the cats and",
        "UQA 韓國 İ 𐐀",
        "23rd new running and",
        "x x x",
    ]
    .into_iter()
    .map(|input| (input, analyzer.analyze_tokens(input).unwrap()))
    .collect();
    let expected = Arc::new(expected);
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let compiled = compiled.clone();
            let expected = expected.clone();
            std::thread::spawn(move || {
                for _ in 0..16 {
                    for (input, expected) in expected.iter() {
                        assert_eq!(&compiled.analyze_tokens(input).unwrap(), expected);
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}
