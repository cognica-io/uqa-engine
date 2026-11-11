//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::AnalyzerLimits;

fn configurations() -> Vec<Analyzer> {
    stages()
        .into_iter()
        .map(|(field, stage)| serde_json::from_value(pipeline(field, stage)).unwrap())
        .chain([
            Analyzer::default(),
            Analyzer::new(
                Tokenizer::Whitespace,
                vec![TokenFilter::Synonym {
                    synonyms: std::collections::BTreeMap::new(),
                    synonyms_path: None,
                }],
                Vec::new(),
            ),
            Analyzer::new(
                Tokenizer::Pattern {
                    pattern: "\"\\\n\r\t\u{8}\u{c}\0\u{1f}é한🙂".into(),
                },
                vec![TokenFilter::Synonym {
                    synonyms: [("\0\nkey".into(), vec!["\"value\\".into(), String::new()])].into(),
                    synonyms_path: Some("\npath한".into()),
                }],
                vec![CharFilter::Mapping {
                    mapping: [("\u{1f}".into(), "\r🙂".into())].into(),
                }],
            ),
        ])
        .collect()
}

#[test]
fn configuration_size_matches_public_serde_limit_diagnostics_for_every_stage() {
    for config in configurations() {
        let bytes = serde_json::to_vec(&config).unwrap();
        for maximum in 0..=bytes.len() + 1 {
            let limits = AnalyzerLimits {
                max_descriptor_bytes: maximum,
                ..Default::default()
            };
            let expected = crate::descriptor::check_configuration(&config, limits)
                .map_err(|error| format!("{error:?}"));
            let actual = inline(check_encoded_size(&config, maximum, &mut ()))
                .map_err(|error| format!("{error:?}"));
            assert_eq!(
                actual,
                expected,
                "limit {maximum}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }
}

fn explicit(config: &Analyzer) -> Value {
    let mut expected = serde_json::to_value(config).unwrap();
    for (value, filter) in expected["token_filters"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(&config.token_filters)
    {
        if let TokenFilter::Synonym {
            synonyms,
            synonyms_path,
        } = filter
        {
            value["synonyms"] = serde_json::to_value(synonyms).unwrap();
            value["synonyms_path"] = serde_json::to_value(synonyms_path).unwrap();
        }
    }
    expected
}

#[test]
fn borrowed_configuration_views_preserve_every_public_serialized_field_and_omission() {
    for config in configurations() {
        let expected = explicit(&config);
        assert!(inline(matches_snapshot(&config, &expected, &mut ())).unwrap());
        let mut modified = expected.clone();
        modified["extra"] = json!(null);
        assert!(!inline(matches_snapshot(&config, &modified, &mut ())).unwrap());
        for field in expected.as_object().unwrap().keys() {
            let mut modified = expected.clone();
            modified.as_object_mut().unwrap().remove(field);
            assert!(
                !inline(matches_snapshot(&config, &modified, &mut ())).unwrap(),
                "missing {field}: {expected}"
            );
        }
    }
}

#[test]
fn configuration_size_retains_long_escape_boundaries_and_structural_comparison() {
    let config = Analyzer::new(
        Tokenizer::Pattern {
            pattern: format!(
                "{}\n{}\u{1f}{}",
                "한🙂".repeat(1200),
                "abc".repeat(2500),
                "x".repeat(5000)
            ),
        },
        Vec::new(),
        Vec::new(),
    );
    let expected = explicit(&config);
    let length = serde_json::to_vec(&config).unwrap().len();
    for maximum in [
        0,
        1,
        32,
        4095,
        4096,
        4097,
        8400,
        10000,
        16000,
        length - 1,
        length,
    ] {
        let limits = AnalyzerLimits {
            max_descriptor_bytes: maximum,
            ..Default::default()
        };
        let reference = crate::descriptor::check_configuration(&config, limits)
            .map_err(|error| error.to_string());
        assert_eq!(
            inline(check_encoded_size(&config, maximum, &mut ()))
                .map_err(|error| error.to_string()),
            reference
        );
    }
    assert!(inline(matches_snapshot(&config, &expected, &mut ())).unwrap());
    let mut changed = expected;
    let mut text = changed["tokenizer"]["pattern"].as_str().unwrap().to_owned();
    text.pop();
    text.push('z');
    changed["tokenizer"]["pattern"] = Value::String(text);
    assert!(!inline(matches_snapshot(&config, &changed, &mut ())).unwrap());
}
