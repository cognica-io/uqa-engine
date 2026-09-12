//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The allocating batch algorithm and scalar substring reference predate lease transfers.

use super::*;

pub(super) fn filter(filter: &TokenFilter, mut input: AnalyzedText) -> AnalyzedText {
    let mut output = Vec::new();
    let mut skipped = 0;
    let mut trailing = None;
    for mut token in input.batch.tokens {
        let keep = match filter {
            TokenFilter::Stop { custom_words, .. } => !token
                .term
                .as_str()
                .is_some_and(|term| custom_words.iter().any(|word| word == term)),
            TokenFilter::Length {
                min_length,
                max_length,
            } => {
                let length = token.term.character_count();
                length >= *min_length && (*max_length == 0 || length <= *max_length)
            }
            _ => true,
        };
        if !keep {
            skipped += token.position_increment;
            trailing = Some(token);
            continue;
        }
        match filter {
            TokenFilter::Synonym { synonyms, .. } => {
                let alternatives = token.term.as_str().and_then(|term| synonyms.get(term));
                output.push(token.clone());
                for term in alternatives.into_iter().flatten() {
                    let mut alternative = token.clone();
                    alternative.replace_term(term.clone().into());
                    alternative.position_increment = 0;
                    output.push(alternative);
                }
            }
            TokenFilter::Ngram {
                min_gram, max_gram, ..
            }
            | TokenFilter::EdgeNgram { min_gram, max_gram } => {
                let boundaries = token.term.boundaries();
                let length = boundaries.len() - 1;
                if length < *min_gram {
                    if matches!(
                        filter,
                        TokenFilter::Ngram {
                            keep_short: true,
                            ..
                        }
                    ) {
                        trailing = None;
                        token.position_increment += skipped;
                        skipped = 0;
                        output.push(token);
                    } else {
                        skipped += token.position_increment;
                        trailing = Some(token);
                    }
                    continue;
                }
                trailing = None;
                let mut first = true;
                for size in *min_gram..=(*max_gram).min(length) {
                    let last = if matches!(filter, TokenFilter::EdgeNgram { .. }) {
                        0
                    } else {
                        length - size
                    };
                    for start in 0..=last {
                        let mut gram = token.substring(boundaries[start]..boundaries[start + size]);
                        gram.position_increment = if first {
                            first = false;
                            let increment = token.position_increment + skipped;
                            skipped = 0;
                            increment
                        } else {
                            0
                        };
                        output.push(gram);
                    }
                }
            }
            _ => {
                trailing = None;
                token.position_increment += skipped;
                skipped = 0;
                output.push(token);
            }
        }
    }
    input.batch.tokens = output;
    if input.batch.terminal.is_none() {
        input.batch.terminal = trailing.map(Box::new);
    }
    input.batch.final_position_increment += skipped;
    input.batch.validate_positions().unwrap();
    input
}
