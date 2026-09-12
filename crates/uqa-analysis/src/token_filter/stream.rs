//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common filters keep the input and output allocation owners through every transformation.

use uqa_core::memory::Budgeted;

use super::compiled::{find_word, PreparedCommonFilter, PreparedTokenFilter};
use crate::token::{allocation::TokenBatchAllocation, TokenBatch};
use crate::{porter, AnalysisResult, AnalyzedText};

mod expand;

pub(super) fn filter(
    filter: &PreparedTokenFilter<'_>,
    batch: TokenBatch,
) -> AnalysisResult<TokenBatch> {
    match filter {
        #[cfg(feature = "nori")]
        PreparedTokenFilter::Nori(filter) => {
            filter.filter_batch(batch, crate::FilteredText::new("").projection())
        }
        PreparedTokenFilter::Common(filter) => Ok(filter
            .filter_batch(
                TokenBatchAllocation::from_unreserved(batch)?,
                &mut || Ok(()),
            )?
            .into_parts()
            .0),
    }
}

impl PreparedCommonFilter<'_> {
    pub(crate) fn filter_analyzed_budgeted(
        &self,
        input: Budgeted<AnalyzedText>,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        let (input, memory) = input.into_parts();
        let (batch, memory) = self
            .filter_batch(
                TokenBatchAllocation::from_budgeted(Budgeted::new(input.batch, memory)),
                poll,
            )?
            .into_parts();
        Ok(Budgeted::new(
            AnalyzedText {
                batch,
                final_offsets: input.final_offsets,
                #[cfg(feature = "nori")]
                projection: input.projection,
            },
            memory,
        ))
    }

    fn filter_batch(
        &self,
        input: TokenBatchAllocation,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<TokenBatch>> {
        let output = match self {
            Self::Lowercase(_) | Self::ASCIIFolding | Self::PorterStem => input.map_terms(
                |token, budget, poll| {
                    Ok(Some(match self {
                        Self::Lowercase(properties) => {
                            super::lowercase::lower_budgeted(&token.term, properties, budget, poll)?
                        }
                        Self::ASCIIFolding => {
                            super::ascii::fold_budgeted(&token.term, budget, poll)?
                        }
                        _ if token.keyword => return Ok(None),
                        _ => porter::stem_term_budgeted(&token.term, budget, poll)?,
                    }))
                },
                poll,
            )?,
            Self::Stop(words) => input.retain(
                |token, poll| match token.term.as_str() {
                    Some(term) => Ok(!find_word(words, term, poll)?),
                    None => Ok(true),
                },
                poll,
            )?,
            Self::Length {
                min_length,
                max_length,
            } => input.retain(
                |token, poll| {
                    let length = token.term.character_count_with_control(poll)?;
                    Ok(length >= *min_length && (*max_length == 0 || length <= *max_length))
                },
                poll,
            )?,
            Self::Synonym(synonyms) => return expand::synonyms(input, synonyms, poll),
            Self::Ngram {
                min_gram,
                max_gram,
                keep_short,
            } => {
                return expand::grams(input, *min_gram, *max_gram, *keep_short, false, poll);
            }
            Self::EdgeNgram { min_gram, max_gram } => {
                return expand::grams(input, *min_gram, *max_gram, false, true, poll);
            }
        };
        output.finish(poll)
    }
}
