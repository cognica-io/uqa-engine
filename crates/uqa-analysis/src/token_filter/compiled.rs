//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared filters retain immutable lookup state and dispatch to their owning algorithms.

use std::borrow::Cow;

use super::{lowercase::prepare_with_profile, validate_gram_bounds, TokenFilter};
use crate::{AnalysisError, AnalysisResult, AnalyzedText};

mod lookup;
pub(super) use lookup::{find_word, PreparedSynonyms};

#[derive(Debug)]
pub(crate) enum PreparedTokenFilter<'a> {
    #[cfg(feature = "nori")]
    Nori(crate::nori::pipeline::PreparedNoriFilter),
    #[cfg(feature = "kuromoji")]
    Kuromoji(std::sync::Arc<crate::kuromoji::pipeline::PreparedKuromojiFilter>),
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
        self.prepare_with_profile(crate::unicode::Profile::default())
    }

    pub(crate) fn prepare_with_profile(
        &self,
        profile: crate::unicode::Profile,
    ) -> AnalysisResult<PreparedTokenFilter<'_>> {
        crate::work::inline(self.prepare_with_profile_and_work(profile, &mut ()))
    }

    pub(crate) async fn prepare_with_profile_and_work(
        &self,
        profile: crate::unicode::Profile,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<PreparedTokenFilter<'_>> {
        work.checkpoint().await?;
        let filter = match self {
            #[cfg(feature = "kuromoji")]
            Self::KuromojiBaseForm(_)
            | Self::KuromojiPartOfSpeech(_)
            | Self::KuromojiStop(_)
            | Self::KuromojiCompletion(_)
            | Self::KuromojiStem(_)
            | Self::KuromojiHiraganaUppercase(_)
            | Self::KuromojiKatakanaUppercase(_)
            | Self::KuromojiReadingForm(_)
            | Self::KuromojiNumber(_) => {
                return crate::kuromoji::pipeline::prepare_filter(
                    self,
                    &crate::kuromoji::KuromojiResources::default(),
                )
                .map(PreparedTokenFilter::Kuromoji);
            }
            #[cfg(feature = "nori")]
            Self::NoriPartOfSpeech(_) | Self::NoriReadingForm(_) | Self::NoriNumber(_) => {
                return crate::nori::pipeline::PreparedNoriFilter::resolve(
                    self,
                    &crate::nori::NoriResources::default(),
                )
                .map(PreparedTokenFilter::Nori);
            }
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            Self::UnicodeSimpleLowercase(config) => {
                #[cfg(not(feature = "kuromoji"))]
                let _ = config;
                #[cfg(not(feature = "nori"))]
                config.unicode_profile.validate_features()?;
                #[cfg(feature = "kuromoji")]
                if config.unicode_profile.kuromoji_dictionary().is_some() {
                    return crate::kuromoji::pipeline::prepare_filter(
                        self,
                        &crate::kuromoji::KuromojiResources::default(),
                    )
                    .map(PreparedTokenFilter::Kuromoji);
                }
                #[cfg(feature = "nori")]
                return crate::nori::pipeline::PreparedNoriFilter::resolve(
                    self,
                    &crate::nori::NoriResources::default(),
                )
                .map(PreparedTokenFilter::Nori);
                #[cfg(not(feature = "nori"))]
                return Err(crate::AnalysisError::Descriptor(
                    "missing Unicode profile provider",
                ));
            }
            Self::Lowercase => PreparedCommonFilter::Lowercase(prepare_with_profile(profile)),
            Self::Stop {
                language,
                custom_words,
            } => {
                PreparedCommonFilter::Stop(lookup::stop_words(language, custom_words, work).await?)
            }
            Self::PorterStem => PreparedCommonFilter::PorterStem,
            Self::ASCIIFolding => PreparedCommonFilter::ASCIIFolding,
            Self::Synonym {
                synonyms,
                synonyms_path,
            } => PreparedCommonFilter::Synonym(match synonyms_path {
                Some(path) => PreparedSynonyms::owned(Self::parse_synonym_file(path)?),
                None => PreparedSynonyms::borrowed_with(synonyms, work).await?,
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
    pub(crate) async fn into_owned_with(
        self,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<PreparedTokenFilter<'static>> {
        Ok(match self {
            #[cfg(feature = "nori")]
            Self::Nori(filter) => PreparedTokenFilter::Nori(filter),
            #[cfg(feature = "kuromoji")]
            Self::Kuromoji(filter) => PreparedTokenFilter::Kuromoji(filter),
            Self::Common(filter) => {
                PreparedTokenFilter::Common(filter.into_owned_with(work).await?)
            }
        })
    }

    pub(crate) fn filter_analyzed(&self, input: AnalyzedText) -> AnalysisResult<AnalyzedText> {
        Ok(self
            .filter_analyzed_budgeted(input.into_unlimited()?, &mut || Ok(()))?
            .into_parts()
            .0)
    }

    pub(crate) fn filter_analyzed_budgeted(
        &self,
        input: uqa_core::memory::Budgeted<AnalyzedText>,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<uqa_core::memory::Budgeted<AnalyzedText>> {
        match self {
            #[cfg(feature = "nori")]
            Self::Nori(filter) => filter.filter_analyzed_budgeted(input, poll),
            #[cfg(feature = "kuromoji")]
            Self::Kuromoji(filter) => filter.filter_analyzed_budgeted(input, poll),
            Self::Common(filter) => filter.filter_analyzed_budgeted(input, poll),
        }
    }
}

impl PreparedCommonFilter<'_> {
    async fn into_owned_with(
        self,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<PreparedCommonFilter<'static>> {
        Ok(match self {
            Self::Lowercase(properties) => PreparedCommonFilter::Lowercase(properties),
            Self::Stop(words) => {
                let mut owned = Vec::with_capacity(words.len());
                for word in words {
                    owned.push(Cow::Owned(crate::work::own_text(word, work).await?));
                }
                PreparedCommonFilter::Stop(owned)
            }
            Self::PorterStem => PreparedCommonFilter::PorterStem,
            Self::ASCIIFolding => PreparedCommonFilter::ASCIIFolding,
            Self::Synonym(synonyms) => {
                PreparedCommonFilter::Synonym(synonyms.into_owned_with(work).await?)
            }
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
        })
    }
}
