//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared filters retain immutable lookup state and dispatch to their owning algorithms.

use std::{borrow::Cow, collections::BTreeSet};

use super::{builtin_stop_words, validate_gram_bounds, TokenFilter};
use crate::{AnalysisResult, AnalyzedText};

mod lookup;
pub(super) use lookup::{find_word, PreparedSynonyms};

#[derive(Debug)]
pub(crate) enum PreparedTokenFilter<'a> {
    #[cfg(feature = "nori")]
    Nori(crate::nori::pipeline::PreparedNoriFilter),
    Common(PreparedCommonFilter<'a>),
}

#[derive(Debug)]
pub(crate) enum PreparedCommonFilter<'a> {
    Lowercase(&'static super::lowercase::CaseProperties),
    Stop(Vec<Cow<'a, str>>),
    PorterStem,
    ASCIIFolding,
    Synonym(PreparedSynonyms<'a>),
    Ngram {
        min_gram: usize,
        max_gram: usize,
        keep_short: bool,
    },
    EdgeNgram {
        min_gram: usize,
        max_gram: usize,
    },
    Length {
        min_length: usize,
        max_length: usize,
    },
}

impl TokenFilter {
    pub(crate) fn prepare(&self) -> AnalysisResult<PreparedTokenFilter<'_>> {
        let filter = match self {
            #[cfg(feature = "nori")]
            Self::NoriPartOfSpeech(_)
            | Self::NoriReadingForm(_)
            | Self::UnicodeSimpleLowercase(_)
            | Self::NoriNumber(_) => {
                return Ok(PreparedTokenFilter::Nori(
                    crate::nori::pipeline::PreparedNoriFilter::resolve(
                        self,
                        &crate::nori::NoriResources::default(),
                    )?,
                ));
            }
            Self::Lowercase => PreparedCommonFilter::Lowercase(super::lowercase::prepare()?),
            Self::Stop {
                language,
                custom_words,
            } => {
                let mut words: BTreeSet<_> = builtin_stop_words(language)
                    .iter()
                    .map(|word| Cow::Borrowed(*word))
                    .collect();
                words.extend(custom_words.iter().map(|word| Cow::Borrowed(word.as_str())));
                PreparedCommonFilter::Stop(words.into_iter().collect())
            }
            Self::PorterStem => PreparedCommonFilter::PorterStem,
            Self::ASCIIFolding => PreparedCommonFilter::ASCIIFolding,
            Self::Synonym {
                synonyms,
                synonyms_path,
            } => PreparedCommonFilter::Synonym(match synonyms_path {
                Some(path) => PreparedSynonyms::owned(Self::parse_synonym_file(path)?),
                None => PreparedSynonyms::borrowed(synonyms),
            }),
            Self::Ngram {
                min_gram,
                max_gram,
                keep_short,
            } => {
                validate_gram_bounds("n-gram token filter", *min_gram, *max_gram)?;
                PreparedCommonFilter::Ngram {
                    min_gram: *min_gram,
                    max_gram: *max_gram,
                    keep_short: *keep_short,
                }
            }
            Self::EdgeNgram { min_gram, max_gram } => {
                validate_gram_bounds("edge n-gram token filter", *min_gram, *max_gram)?;
                PreparedCommonFilter::EdgeNgram {
                    min_gram: *min_gram,
                    max_gram: *max_gram,
                }
            }
            Self::Length {
                min_length,
                max_length,
            } => PreparedCommonFilter::Length {
                min_length: *min_length,
                max_length: *max_length,
            },
        };
        Ok(PreparedTokenFilter::Common(filter))
    }
}

impl PreparedTokenFilter<'_> {
    pub(crate) fn into_owned(self) -> PreparedTokenFilter<'static> {
        match self {
            #[cfg(feature = "nori")]
            Self::Nori(filter) => PreparedTokenFilter::Nori(filter),
            Self::Common(filter) => PreparedTokenFilter::Common(filter.into_owned()),
        }
    }

    pub(crate) fn filter_analyzed(&self, input: AnalyzedText) -> AnalysisResult<AnalyzedText> {
        match self {
            #[cfg(feature = "nori")]
            Self::Nori(filter) => filter.filter_analyzed(input),
            Self::Common(filter) => Ok(filter
                .filter_analyzed_budgeted(input.into_unlimited()?, &mut || Ok(()))?
                .into_parts()
                .0),
        }
    }
}

impl PreparedCommonFilter<'_> {
    fn into_owned(self) -> PreparedCommonFilter<'static> {
        match self {
            Self::Lowercase(properties) => PreparedCommonFilter::Lowercase(properties),
            Self::Stop(words) => PreparedCommonFilter::Stop(
                words
                    .into_iter()
                    .map(|word| Cow::Owned(word.into_owned()))
                    .collect(),
            ),
            Self::PorterStem => PreparedCommonFilter::PorterStem,
            Self::ASCIIFolding => PreparedCommonFilter::ASCIIFolding,
            Self::Synonym(synonyms) => PreparedCommonFilter::Synonym(synonyms.into_owned()),
            Self::Ngram {
                min_gram,
                max_gram,
                keep_short,
            } => PreparedCommonFilter::Ngram {
                min_gram,
                max_gram,
                keep_short,
            },
            Self::EdgeNgram { min_gram, max_gram } => {
                PreparedCommonFilter::EdgeNgram { min_gram, max_gram }
            }
            Self::Length {
                min_length,
                max_length,
            } => PreparedCommonFilter::Length {
                min_length,
                max_length,
            },
        }
    }
}
