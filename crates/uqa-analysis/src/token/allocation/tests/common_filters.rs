//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::token_filter::PreparedTokenFilter;

mod reference;
mod resources;

#[cfg(feature = "nori")]
mod nori;

fn apply(
    filter: &TokenFilter,
    input: Budgeted<AnalyzedText>,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<AnalyzedText>> {
    match filter.prepare()? {
        PreparedTokenFilter::Common(filter) => filter.filter_analyzed_budgeted(input, poll),
        #[cfg(feature = "nori")]
        PreparedTokenFilter::Nori(_) => panic!("common filter expected"),
    }
}

fn stop(words: &[&str]) -> TokenFilter {
    TokenFilter::Stop {
        language: String::new(),
        custom_words: words.iter().map(|word| (*word).into()).collect(),
    }
}

fn synonyms() -> TokenFilter {
    TokenFilter::Synonym {
        synonyms: [
            (
                "UQA".into(),
                vec!["UQA".into(), "uqa".into(), "韓🙂".into()],
            ),
            ("the".into(), vec![String::new(), "UQA".into()]),
            (String::new(), vec!["empty".into()]),
        ]
        .into(),
        synonyms_path: None,
    }
}

fn input() -> AnalyzedText {
    let mut input = Tokenizer::Whitespace
        .tokenize_with_offsets("UQA the 🙂x ab Àrunning ÀΟΣ")
        .unwrap();
    let raw = attributes().batch.tokens.remove(0);
    input.batch.tokens.push(raw);
    input.batch.tokens[1].position_increment = 0;
    input.batch.tokens[2].position_increment = 3;
    input.batch.tokens[3].position_length = 2;
    input.batch.tokens[4].keyword = true;
    #[cfg(feature = "nori")]
    {
        let morphology = input.batch.tokens.last().unwrap().korean_morphology.clone();
        for token in &mut input.batch.tokens[..6] {
            token.korean_morphology = morphology.clone();
        }
    }
    input.batch.final_position_increment = 4;
    input
}

fn assert_owned(output: Budgeted<AnalyzedText>, budget: &MemoryBudget, unrelated: usize) {
    assert_eq!(budget.used(), output.reserved_bytes() + unrelated);
    let (output, memory) = output.into_parts();
    assert_eq!(owned_bytes(output), memory.bytes());
    assert_eq!(budget.used(), memory.bytes() + unrelated);
    drop(memory);
    assert_eq!(budget.used(), unrelated);
}

#[test]
fn common_filter_graphs_match_allocating_reference_across_removal_and_expansion() {
    let filters = [
        stop(&["the", "ab", "ÀΟΣ"]),
        TokenFilter::Length {
            min_length: 3,
            max_length: 0,
        },
        TokenFilter::Length {
            min_length: 1,
            max_length: 2,
        },
        synonyms(),
        TokenFilter::Ngram {
            min_gram: 1,
            max_gram: 3,
            keep_short: false,
        },
        TokenFilter::Ngram {
            min_gram: 2,
            max_gram: 4,
            keep_short: true,
        },
        TokenFilter::Ngram {
            min_gram: 4,
            max_gram: 4,
            keep_short: false,
        },
        TokenFilter::EdgeNgram {
            min_gram: 2,
            max_gram: 5,
        },
    ];
    let original = input();
    for mask in 0..128 {
        for existing_terminal in [false, true] {
            let mut input = original.clone();
            input.batch.tokens = input
                .batch
                .tokens
                .into_iter()
                .enumerate()
                .filter_map(|(index, token)| (mask & (1 << index) != 0).then_some(token))
                .collect();
            if let Some(first) = input.batch.tokens.first_mut() {
                first.position_increment = 1;
            }
            if existing_terminal {
                input.batch.terminal = Some(Box::new(original.batch.tokens[1].clone()));
            }
            for filter in &filters {
                let expected = reference::filter(filter, input.clone());
                let budget = MemoryBudget::new(1 << 20);
                let input = input.clone_budgeted(&budget, || Ok(())).unwrap();
                let output = apply(filter, input, &mut || Ok(())).unwrap();
                assert_eq!(
                    *output, expected,
                    "{filter:?}, mask {mask}, terminal {existing_terminal}"
                );
                assert_owned(output, &budget, 0);
            }
        }
    }
}

#[test]
fn common_term_replacements_keep_attributes_and_only_change_the_active_terms() {
    for (filter, expected) in [
        (
            TokenFilter::Lowercase,
            ["uqa", "the", "🙂x", "ab", "àrunning", "àος"],
        ),
        (
            TokenFilter::ASCIIFolding,
            ["UQA", "the", "🙂x", "ab", "Arunning", "AΟΣ"],
        ),
        (
            TokenFilter::PorterStem,
            ["UQA", "the", "🙂x", "ab", "Àrunning", "ÀΟΣ"],
        ),
    ] {
        let mut original = input();
        original.batch.terminal = Some(Box::new(original.batch.tokens[0].clone()));
        let mut expected_input = original.clone();
        for (token, expected) in expected_input.batch.tokens.iter_mut().zip(expected) {
            token.replace_term(expected.into());
        }
        let budget = MemoryBudget::new(1 << 20);
        let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
        let output = apply(&filter, input, &mut || Ok(())).unwrap();
        assert_eq!(*output, expected_input, "{filter:?}");
        assert_owned(output, &budget, 0);
    }
}
