//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::nori::{
    DecompoundMode, KoreanTokenizer, NoriLimits, NoriOptions, NoriResources, UserDictionary,
    UserDictionaryLimits,
};

#[test]
fn common_chain_retains_native_morphology_and_source_leases_after_every_stage() {
    let resources = NoriResources::default().load_default().unwrap();
    let user = UserDictionary::compile(
        "🙂a 가 나",
        resources.model(),
        UserDictionaryLimits::default(),
    )
    .unwrap();
    let tokenizer = KoreanTokenizer::new(
        resources.model().clone(),
        user,
        NoriOptions {
            decompound_mode: DecompoundMode::Mixed,
            ..NoriOptions::default()
        },
    )
    .unwrap();
    let source = crate::FilteredText::new("韓國 감싸여 🙂a UQA 원 z");
    let budget = MemoryBudget::new(1 << 24);
    let other = budget.reserve(7).unwrap();
    source.prepare_coordinates(&budget, &mut || Ok(())).unwrap();
    let native = tokenizer
        .tokenize_budgeted(source.as_str(), NoriLimits::default(), &budget, &mut || {
            Ok(())
        })
        .unwrap();
    let mut output = AnalyzedText::from_nori_budgeted(native, &source, &mut || Ok(())).unwrap();
    assert!(output.tokens().iter().any(|token| token
        .korean_morphology()
        .unwrap()
        .reading
        .is_some()));
    assert!(output.tokens().iter().any(|token| token
        .korean_morphology()
        .unwrap()
        .morphemes
        .is_some()));
    assert!(output
        .tokens()
        .iter()
        .any(|token| token.term().as_str().is_none()));
    drop(source);
    let source_bytes = budget.used() - output.reserved_bytes() - 7;
    assert!(source_bytes > 0);
    let projection = std::sync::Arc::as_ptr(&output.projection);
    let synonyms = output
        .tokens()
        .iter()
        .filter_map(|token| token.term().as_str())
        .map(|term| (term.to_owned(), vec![term.to_owned(), "韓🙂".into()]))
        .collect();
    for filter in [
        stop(&["원", "z"]),
        TokenFilter::Synonym {
            synonyms,
            synonyms_path: None,
        },
        TokenFilter::Ngram {
            min_gram: 1,
            max_gram: 2,
            keep_short: false,
        },
        TokenFilter::Length {
            min_length: 2,
            max_length: 0,
        },
    ] {
        let expected = reference::filter(&filter, (*output).clone());
        output = apply(&filter, output, &mut || Ok(())).unwrap();
        assert_eq!(*output, expected);
        assert_eq!(std::sync::Arc::as_ptr(&output.projection), projection);
        assert_eq!(budget.used(), output.reserved_bytes() + source_bytes + 7);
        assert!(output
            .tokens()
            .iter()
            .all(|token| token.korean_morphology().is_some()));
    }
    drop(output);
    assert_eq!(budget.used(), 7);
    drop(other);
}
