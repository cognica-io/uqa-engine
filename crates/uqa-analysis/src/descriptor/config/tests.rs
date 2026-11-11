//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{CharFilter, Tokenizer};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("resolved configuration validation borrows its original value")
    }
}

#[test]
fn resolved_configuration_validation_retains_work_without_complete_pipeline_copies() {
    let config = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Synonym {
            synonyms: (0..256)
                .map(|i| {
                    (
                        format!("{i:04}{}", "한🙂".repeat(128)),
                        vec!["value".repeat(819), "尾".repeat(1024)],
                    )
                })
                .collect(),
            synonyms_path: None,
        }],
        vec![CharFilter::Mapping {
            mapping: [("source".into(), "target".repeat(1024))].into(),
        }],
    );
    let limits = AnalyzerLimits::default();
    let value = Arc::new(snapshot(&config, limits).unwrap());
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let source = Arc::clone(&value);
    let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
        restore(&source, limits, &mut channel.input()).await
    })
    .unwrap();
    let mut calls = 0;
    let actual = loop {
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap();
        });
        assert!(
            allocation.bytes_total < 64 * 1024,
            "configuration validation poll {calls} allocated {} bytes",
            allocation.bytes_total
        );
        calls += 1;
        if let Some(result) = result {
            break result;
        }
        assert!(calls < 100_000);
    };
    assert!(calls > 1024);
    assert_eq!(snapshot(&actual, limits).unwrap(), *value);
    drop(input);
    assert_eq!(Arc::strong_count(&value), 1);
    assert_eq!(memory.used(), 0);
}

fn reference(value: &Value, limits: AnalyzerLimits) -> AnalysisResult<Analyzer> {
    let config: Analyzer = serde_json::from_value(value.clone())?;
    check_config(&config, limits)?;
    #[cfg(feature = "nori")]
    crate::nori::pipeline::check_resolved(&config)?;
    #[cfg(feature = "kuromoji")]
    crate::kuromoji::pipeline::check_resolved(&config)?;
    for filter in &config.token_filters {
        if matches!(
            filter,
            TokenFilter::Synonym {
                synonyms_path: Some(_),
                ..
            }
        ) {
            return Err(invalid("resolved synonyms must not contain a file path"));
        }
    }
    if canonical(value) != snapshot(&config, limits)? {
        return Err(invalid(
            "pipeline contains unknown properties, unresolved values, or implicit defaults",
        ));
    }
    Ok(config)
}

#[test]
fn resolved_configuration_validation_preserves_canonical_fields_and_error_order() {
    use serde_json::json;
    let limits = AnalyzerLimits::default();
    #[allow(unused_mut)]
    let mut filters = vec![
        json!({"type":"stop","language":"","custom_words":[]}),
        json!({"type":"stop","language":"","custom_words":["a","z","한"]}),
        json!({"type":"stop","language":"","custom_words":["a","a"]}),
        json!({"type":"stop","language":"","custom_words":["z","a"]}),
        json!({"type":"stop","language":"english","custom_words":["z","a"]}),
        json!({"type":"stop","custom_words":[]}),
        json!({"type":"synonym","synonyms":{},"synonyms_path":null}),
        json!({"type":"synonym","synonyms_path":null}),
        json!({"type":"synonym","synonyms":{"word":["one","two"]}}),
        json!({"type":"synonym","synonyms_path":"/unavailable/never-read.synonyms"}),
        json!({"type":"ngram","min_gram":0,"max_gram":0,"keep_short":false}),
        json!({"type":"ascii_folding"}),
        json!({"type":"a_s_c_i_i_folding"}),
        json!({"type":"lowercase","unknown":true}),
        json!(["stop", "", []]),
    ];
    #[cfg(feature = "nori")]
    filters.extend([
        json!({"type":"nori_part_of_speech","stop_tags":null}),
        json!({"type":"nori_part_of_speech","stop_tags":["NNG","NNP"]}),
        json!({"type":"nori_part_of_speech","stop_tags":["NNP","NNG"]}),
        json!({"type":"nori_part_of_speech","stop_tags":["NNG","NNG"]}),
        json!({"type":"unicode_simple_lowercase","unicode_profile":"jdk21"}),
        json!({"type":"unicode_simple_lowercase","unicode_profile":format!("sha256:{}","a".repeat(64))}),
    ]);
    #[cfg(feature = "kuromoji")]
    filters.extend([
        json!({"type":"kuromoji_part_of_speech","stop_tags":null,"dictionary":null}),
        json!({"type":"kuromoji_part_of_speech","stop_tags":["a","b"],"dictionary":null}),
        json!({"type":"kuromoji_part_of_speech","stop_tags":["b","a"],"dictionary":null}),
        json!({"type":"kuromoji_stop","words":["a","a"],"ignore_case":false,"dictionary":null}),
        json!({"type":"kuromoji_stop","words":["a","b"],"ignore_case":false,"dictionary":null}),
        json!({"type":"kuromoji_stop","words":null,"ignore_case":false,"dictionary":null}),
        json!({"type":"kuromoji_stemmer","minimum_length":-1}),
    ]);
    let mut values = vec![json!({}), json!([{"type":"whitespace"},[],[],null])];
    for filter in filters {
        values.push(
            json!({"tokenizer":{"type":"whitespace"},"token_filters":[filter],"char_filters":[]}),
        );
    }
    values.push(json!({"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"stop","language":"english","custom_words":[]},{"type":"ngram","min_gram":0,"max_gram":0,"keep_short":false}],"char_filters":[]}));
    let base = snapshot(&Analyzer::default(), limits).unwrap();
    for field in ["normalization", "extra"] {
        let mut value = base.clone();
        value[field] = Value::Null;
        values.push(value);
    }
    for value in values {
        for limits in [
            limits,
            AnalyzerLimits {
                max_descriptor_bytes: 30,
                ..limits
            },
            AnalyzerLimits {
                max_stages: 1,
                ..limits
            },
        ] {
            let expected = reference(&value, limits)
                .map(|config| serde_json::to_value(config).unwrap())
                .map_err(|error| format!("{error:?}"));
            let actual = crate::work::inline(restore(&value, limits, &mut ()))
                .map(|config| serde_json::to_value(config).unwrap())
                .map_err(|error| format!("{error:?}"));
            assert_eq!(actual, expected, "{value}");
        }
    }
}

#[test]
fn resolved_configuration_validation_retires_each_suspended_owner() {
    let config = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Stop {
            language: String::new(),
            custom_words: (0..256)
                .map(|index| format!("{}-{index:04}", "한🙂".repeat(512)))
                .collect(),
        }],
        Vec::new(),
    );
    let value = Arc::new(snapshot(&config, AnalyzerLimits::default()).unwrap());
    let mut total = 0;
    for (stop, action) in std::iter::once((usize::MAX, 0)).chain(
        [0, 1, 2, 3, 4]
            .into_iter()
            .flat_map(|fraction| (0..3).map(move |action| (fraction, action))),
    ) {
        let stop = if stop == usize::MAX {
            stop
        } else {
            total * stop / 5
        };
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let source = Arc::clone(&value);
        let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
            restore(&source, AnalyzerLimits::default(), &mut channel.input()).await
        })
        .unwrap();
        let mut calls = 0;
        while calls < stop {
            if input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_some()
            {
                assert_eq!(stop, usize::MAX);
                total = calls;
                break;
            }
            calls += 1;
            assert!(calls < 100_000);
        }
        if stop != usize::MAX {
            match action {
                0 => (),
                1 => {
                    token.cancel();
                    assert!(input
                        .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                        .is_err());
                    assert_eq!(Arc::strong_count(&value), 1);
                    assert_eq!(memory.used(), 0);
                }
                2 => {
                    assert!(input
                        .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 0 })
                        .is_err());
                    assert_eq!(Arc::strong_count(&value), 1);
                    assert_eq!(memory.used(), 0);
                }
                _ => unreachable!(),
            }
        }
        drop(input);
        assert_eq!(Arc::strong_count(&value), 1);
        assert_eq!(memory.used(), 0);
    }
    assert!(total > 2048);
}
