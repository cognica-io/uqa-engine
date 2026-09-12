//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared filter state borrows live configuration or owns a complete immutable snapshot.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use super::{builtin_stop_words, stream, validate_gram_bounds, TokenFilter};
use crate::{AnalysisResult, AnalyzedText};

#[derive(Debug)]
pub(crate) enum PreparedTokenFilter<'a> {
    #[cfg(feature = "nori")]
    Nori(crate::nori::pipeline::PreparedNoriFilter),
    Lowercase,
    Stop(BTreeSet<Cow<'a, str>>),
    PorterStem,
    ASCIIFolding,
    Synonym(Cow<'a, BTreeMap<String, Vec<String>>>),
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
        Ok(match self {
            #[cfg(feature = "nori")]
            Self::NoriPartOfSpeech(_)
            | Self::NoriReadingForm(_)
            | Self::UnicodeSimpleLowercase(_)
            | Self::NoriNumber(_) => {
                PreparedTokenFilter::Nori(crate::nori::pipeline::PreparedNoriFilter::resolve(
                    self,
                    &crate::nori::NoriResources::default(),
                )?)
            }
            Self::Lowercase => PreparedTokenFilter::Lowercase,
            Self::Stop {
                language,
                custom_words,
            } => {
                let mut words: BTreeSet<_> = builtin_stop_words(language)
                    .iter()
                    .map(|word| Cow::Borrowed(*word))
                    .collect();
                words.extend(custom_words.iter().map(|word| Cow::Borrowed(word.as_str())));
                PreparedTokenFilter::Stop(words)
            }
            Self::PorterStem => PreparedTokenFilter::PorterStem,
            Self::ASCIIFolding => PreparedTokenFilter::ASCIIFolding,
            Self::Synonym {
                synonyms,
                synonyms_path,
            } => PreparedTokenFilter::Synonym(match synonyms_path {
                Some(path) => Cow::Owned(Self::parse_synonym_file(path)?),
                None => Cow::Borrowed(synonyms),
            }),
            Self::Ngram {
                min_gram,
                max_gram,
                keep_short,
            } => {
                validate_gram_bounds("n-gram token filter", *min_gram, *max_gram)?;
                PreparedTokenFilter::Ngram {
                    min_gram: *min_gram,
                    max_gram: *max_gram,
                    keep_short: *keep_short,
                }
            }
            Self::EdgeNgram { min_gram, max_gram } => {
                validate_gram_bounds("edge n-gram token filter", *min_gram, *max_gram)?;
                PreparedTokenFilter::EdgeNgram {
                    min_gram: *min_gram,
                    max_gram: *max_gram,
                }
            }
            Self::Length {
                min_length,
                max_length,
            } => PreparedTokenFilter::Length {
                min_length: *min_length,
                max_length: *max_length,
            },
        })
    }
}

impl PreparedTokenFilter<'_> {
    pub(crate) fn into_owned(self) -> PreparedTokenFilter<'static> {
        match self {
            #[cfg(feature = "nori")]
            Self::Nori(filter) => PreparedTokenFilter::Nori(filter),
            Self::Lowercase => PreparedTokenFilter::Lowercase,
            Self::Stop(words) => PreparedTokenFilter::Stop(
                words
                    .into_iter()
                    .map(|word| Cow::Owned(word.into_owned()))
                    .collect(),
            ),
            Self::PorterStem => PreparedTokenFilter::PorterStem,
            Self::ASCIIFolding => PreparedTokenFilter::ASCIIFolding,
            Self::Synonym(synonyms) => {
                PreparedTokenFilter::Synonym(Cow::Owned(synonyms.into_owned()))
            }
            Self::Ngram {
                min_gram,
                max_gram,
                keep_short,
            } => PreparedTokenFilter::Ngram {
                min_gram,
                max_gram,
                keep_short,
            },
            Self::EdgeNgram { min_gram, max_gram } => {
                PreparedTokenFilter::EdgeNgram { min_gram, max_gram }
            }
            Self::Length {
                min_length,
                max_length,
            } => PreparedTokenFilter::Length {
                min_length,
                max_length,
            },
        }
    }

    pub(crate) fn filter_analyzed(&self, mut input: AnalyzedText) -> AnalysisResult<AnalyzedText> {
        #[cfg(feature = "nori")]
        if let Self::Nori(filter) = self {
            return filter.filter_analyzed(input);
        }
        input.batch = stream::filter(self, input.batch)?;
        Ok(input)
    }
}
