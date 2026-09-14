//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{CharFilter, TokenFilter, TokenLengthPolicy};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_core::memory::MemoryBudget;

fn pipeline() -> Analyzer {
    Analyzer::new(
        Tokenizer::Kuromoji(KuromojiTokenizerConfig::default()),
        Vec::new(),
        Vec::new(),
    )
}

#[test]
fn japanese_tokenizer_configuration_freezes_defaults_without_inferred_normalization() {
    let config: Analyzer =
        serde_json::from_value(json!({"tokenizer": {"type": "kuromoji_tokenizer"}})).unwrap();
    let compiled = config.compile().unwrap();
    assert!(config.uses_japanese_stages());
    assert!(config.uses_morphology_stages());
    assert!(!config.uses_korean_stages());
    assert_eq!(
        compiled.descriptor().length_policy(),
        TokenLengthPolicy::DiscountOverlaps
    );
    let wire: Value = serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
    assert_eq!(
        wire["descriptor"]["pipeline"]["tokenizer"],
        json!({
            "type": "kuromoji_tokenizer", "dictionary": format!("sha256:{}", uqa_kuromoji_data::BUNDLE_SHA256),
            "mode": "search", "discard_punctuation": true, "discard_compound_token": true,
            "user_dictionary": null, "n_best_cost": 0, "n_best_examples": null,
        })
    );
    assert!(matches!(
        compiled.normalize("ＵＱＡ"),
        Err(AnalysisError::NormalizationUnavailable)
    ));
    assert_eq!(
        config.analyze_tokens("関西国際空港").unwrap(),
        compiled.analyze_tokens("関西国際空港").unwrap()
    );
    assert_eq!(
        config
            .tokenizer
            .tokenize_with_offsets("関西国際空港")
            .unwrap(),
        compiled.analyze_tokens("関西国際空港").unwrap()
    );
    for (field, value) in [
        ("extra", json!(true)),
        ("mode", json!("unknown")),
        ("n_best_cost", json!(i64::MAX)),
    ] {
        let mut config = json!({"type": "kuromoji_tokenizer"});
        config[field] = value;
        assert!(serde_json::from_value::<Tokenizer>(config).is_err());
    }
}

#[test]
fn example_preparation_freezes_effective_cost_and_exact_user_source_identity() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let mut config = pipeline();
    let Tokenizer::Kuromoji(tokenizer) = &mut config.tokenizer else {
        unreachable!()
    };
    tokenizer.n_best_cost = 2000;
    tokenizer.n_best_examples = Some("関西国際空港-関西".into());
    let original = serde_json::to_value(&config).unwrap();
    let compiled = resources.compile(&config).unwrap();
    assert_eq!(serde_json::to_value(&config).unwrap(), original);
    let mut resolved = compiled.descriptor().configuration().unwrap();
    let Tokenizer::Kuromoji(tokenizer) = &mut resolved.tokenizer else {
        unreachable!()
    };
    assert_eq!(tokenizer.n_best_cost, 9325);
    assert_eq!(tokenizer.n_best_examples, None);
    assert!(Arc::ptr_eq(
        &compiled,
        &resources.compile(&resolved).unwrap()
    ));
    let reopened = AnalyzerResources::new(AnalyzerLimits::default())
        .restore_json(compiled.descriptor().canonical_json())
        .unwrap();
    assert_eq!(
        reopened.analyze_tokens("関西国際空港").unwrap(),
        compiled.analyze_tokens("関西国際空港").unwrap()
    );
    let mut identities = Vec::new();
    for user in [
        None,
        Some(""),
        Some("# empty\n"),
        Some("東京大学,東京 大学,トウキョウ ダイガク,名詞"),
        Some("東京大学,東京 大学,トウキョウ ダイガク,名詞\n"),
    ] {
        let Tokenizer::Kuromoji(tokenizer) = &mut resolved.tokenizer else {
            unreachable!()
        };
        tokenizer.user_dictionary = user.map(str::to_owned);
        identities.push(
            resources
                .compile(&resolved)
                .unwrap()
                .descriptor()
                .fingerprint(),
        );
    }
    identities.sort();
    identities.dedup();
    assert_eq!(identities.len(), 5);
    for examples in ["malformed".to_owned(), "a-b/".repeat(1025)] {
        let fresh = AnalyzerResources::new(AnalyzerLimits::default());
        let Tokenizer::Kuromoji(tokenizer) = &mut config.tokenizer else {
            unreachable!()
        };
        tokenizer.n_best_examples = Some(examples);
        assert!(fresh.compile(&config).is_err());
        assert_eq!(fresh.cache_stats().analyzers, 0);
    }
}

#[test]
fn restored_japanese_tokenizers_reject_unresolved_examples_aliases_and_implicit_defaults() {
    let compiled = pipeline().compile().unwrap();
    let original: Value = serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
    for field in ["n_best_examples", "dictionary", "discard_compound_token"] {
        let mut wire = original.clone();
        let tokenizer = &mut wire["descriptor"]["pipeline"]["tokenizer"];
        match field {
            "n_best_examples" => tokenizer[field] = json!("malformed"),
            "dictionary" => tokenizer[field] = json!("lucene-10.5.1"),
            _ => {
                tokenizer.as_object_mut().unwrap().remove(field);
            }
        }
        let mut hash = Sha256::new();
        hash.update(b"UQA analyzer descriptor\0");
        hash.update(serde_json::to_vec(&wire["descriptor"]).unwrap());
        wire["fingerprint"] = json!(format!("{:x}", hash.finalize()));
        let resources = AnalyzerResources::new(AnalyzerLimits::default());
        assert!(
            resources.restore_json(&wire.to_string()).is_err(),
            "{field}"
        );
        assert_eq!(resources.cache_stats().analyzers, 0);
    }
}

#[test]
fn common_filters_preserve_deferred_japanese_attribute_failures_until_public_emission() {
    let mut config = pipeline();
    let Tokenizer::Kuromoji(tokenizer) = &mut config.tokenizer else {
        unreachable!()
    };
    tokenizer.user_dictionary = Some("東京,東京,トウキョウ,".into());
    assert!(config.tokenizer.tokenize_with_offsets("東京").is_err());
    assert!(config.analyze_tokens("東京").is_err());
    assert!(config.compile().unwrap().analyze_tokens("東京").is_err());
    config.token_filters.push(TokenFilter::Lowercase);
    config.token_filters.push(TokenFilter::Stop {
        language: String::new(),
        custom_words: vec!["東京".into()],
    });
    let compiled = config.compile().unwrap();
    assert!(compiled.analyze_tokens("東京").unwrap().tokens().is_empty());
    assert_eq!(
        config.analyze_tokens("東京").unwrap(),
        compiled.analyze_tokens("東京").unwrap()
    );
}

#[test]
fn compiled_japanese_graphs_keep_source_and_release_every_cancelled_or_bounded_run() {
    let mut config = pipeline();
    let Tokenizer::Kuromoji(tokenizer) = &mut config.tokenizer else {
        unreachable!()
    };
    tokenizer.discard_compound_token = false;
    tokenizer.n_best_examples = Some("関西国際空港-関西".into());
    config.char_filters = vec![CharFilter::HTMLStrip, CharFilter::CJKWidth];
    config.token_filters = vec![TokenFilter::UnicodeSimpleLowercase(
        crate::SimpleLowercaseConfig {
            unicode_profile: UnicodeProfile::Kuromoji {
                dictionary: "lucene-10.5.1".into(),
            }
            .into(),
        },
    )];
    let compiled = config.compile().unwrap();
    let source = "<b>関西国際空港 ＵＱＡ</b>";
    let budget = MemoryBudget::new(usize::MAX);
    let mut calls = 0;
    let expected = compiled
        .analyze_tokens_budgeted(source, &budget, || {
            calls += 1;
            Ok(())
        })
        .unwrap();
    assert!(expected
        .tokens()
        .iter()
        .any(|token| token.position_length() > 1));
    let token = expected
        .tokens()
        .iter()
        .find(|token| token.term() == "uqa")
        .unwrap();
    assert_eq!(&source[token.offsets().unwrap().utf8.clone()], "ＵＱＡ");
    for cutoff in 1..=calls {
        let budget = MemoryBudget::new(usize::MAX);
        let mut count = 0;
        assert!(matches!(
            compiled.analyze_tokens_budgeted(source, &budget, || {
                count += 1;
                if count == cutoff {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(AnalysisError::Cancelled)
        ));
        assert_eq!(budget.used(), 0, "callback {cutoff}");
    }
    let peak = budget.peak();
    let mut failures = 0;
    for limit in (0..peak).step_by((peak / 17).max(1)).chain([peak]) {
        let budget = MemoryBudget::new(limit + 7);
        let unrelated = budget.reserve(7).unwrap();
        match compiled.analyze_tokens_budgeted(source, &budget, || Ok(())) {
            Ok(output) => {
                assert_eq!(*output, *expected);
                drop(output);
            }
            Err(AnalysisError::Memory(_)) => failures += 1,
            output => panic!("unexpected budget outcome: {output:?}"),
        }
        assert_eq!(budget.used(), 7);
        drop(unrelated);
        assert_eq!(budget.used(), 0);
    }
    assert!(failures > 0);
    drop(expected);
    assert_eq!(budget.used(), 0);
}
