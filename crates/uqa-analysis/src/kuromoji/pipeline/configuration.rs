//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese filter snapshots retain expanded sets and only profiles used during execution.

use super::super::{JapaneseFilter, ResolvedDictionary, DEFAULT_KUROMOJI_DICTIONARY};
use crate::{AnalysisError, AnalysisResult, Analyzer, TokenFilter};

pub(crate) fn is_japanese_filter(filter: &TokenFilter) -> bool {
    match filter {
        TokenFilter::KuromojiBaseForm(_)
        | TokenFilter::KuromojiPartOfSpeech(_)
        | TokenFilter::KuromojiStop(_)
        | TokenFilter::KuromojiStem(_)
        | TokenFilter::KuromojiHiraganaUppercase(_)
        | TokenFilter::KuromojiKatakanaUppercase(_)
        | TokenFilter::KuromojiReadingForm(_)
        | TokenFilter::KuromojiNumber(_)
        | TokenFilter::KuromojiCompletion(_) => true,
        TokenFilter::UnicodeSimpleLowercase(config) => {
            config.unicode_profile.kuromoji_dictionary().is_some()
        }
        _ => false,
    }
}

pub(super) fn dictionary_name(filter: &TokenFilter) -> AnalysisResult<Option<&str>> {
    match filter {
        TokenFilter::KuromojiPartOfSpeech(config) => {
            set_dictionary(config.stop_tags.is_none(), config.dictionary.as_deref())
        }
        TokenFilter::KuromojiStop(config) => set_dictionary(
            config.words.is_none() || config.ignore_case,
            config.dictionary.as_deref(),
        ),
        TokenFilter::KuromojiCompletion(config) => Ok(Some(&config.dictionary)),
        TokenFilter::UnicodeSimpleLowercase(config) => {
            Ok(config.unicode_profile.kuromoji_dictionary())
        }
        _ => Ok(None),
    }
}

fn set_dictionary(needed: bool, dictionary: Option<&str>) -> AnalysisResult<Option<&str>> {
    if needed {
        Ok(Some(dictionary.unwrap_or(DEFAULT_KUROMOJI_DICTIONARY)))
    } else if dictionary.is_some() {
        Err(AnalysisError::Descriptor(
            "explicit Japanese stop sets must not specify an unused dictionary",
        ))
    } else {
        Ok(None)
    }
}

pub(super) fn native_filter(filter: &TokenFilter) -> AnalysisResult<JapaneseFilter> {
    Ok(match filter {
        TokenFilter::KuromojiBaseForm(_) => JapaneseFilter::BaseForm,
        TokenFilter::KuromojiPartOfSpeech(config) => JapaneseFilter::PartOfSpeech {
            stop_tags: config.stop_tags.clone(),
        },
        TokenFilter::KuromojiStop(config) => JapaneseFilter::Stop {
            words: config.words.clone(),
            ignore_case: config.ignore_case,
        },
        TokenFilter::KuromojiStem(config) => JapaneseFilter::KatakanaStem {
            minimum_length: config.minimum_length,
        },
        TokenFilter::KuromojiHiraganaUppercase(_) => JapaneseFilter::HiraganaUppercase,
        TokenFilter::KuromojiKatakanaUppercase(_) => JapaneseFilter::KatakanaUppercase,
        TokenFilter::KuromojiReadingForm(config) => JapaneseFilter::ReadingForm {
            use_romaji: config.use_romaji,
        },
        TokenFilter::KuromojiNumber(_) => JapaneseFilter::Number,
        TokenFilter::KuromojiCompletion(config) => JapaneseFilter::Completion { mode: config.mode },
        TokenFilter::UnicodeSimpleLowercase(config)
            if config.unicode_profile.kuromoji_dictionary().is_some() =>
        {
            JapaneseFilter::SimpleLowercase
        }
        _ => return Err(AnalysisError::Descriptor("expected a Japanese filter")),
    })
}

/// Freeze after native preparation has validated set size and Unicode behavior.
pub(super) fn freeze(
    filter: &mut TokenFilter,
    profile: Option<&ResolvedDictionary>,
) -> AnalysisResult<bool> {
    let expanded = match filter {
        TokenFilter::KuromojiPartOfSpeech(config) => {
            let expanded = config.stop_tags.is_none();
            if expanded {
                config.stop_tags = Some(required(profile)?.model().default_stop_tags().to_vec());
            }
            config.dictionary = None;
            expanded
        }
        TokenFilter::KuromojiStop(config) => {
            let expanded = config.words.is_none();
            if expanded {
                config.words = Some(required(profile)?.model().default_stop_words().to_vec());
            }
            config.dictionary = if config.ignore_case {
                Some(exact_name(required(profile)?))
            } else {
                None
            };
            expanded
        }
        TokenFilter::KuromojiCompletion(config) => {
            config.dictionary = exact_name(required(profile)?);
            false
        }
        TokenFilter::UnicodeSimpleLowercase(config) => {
            if let Some(dictionary) = config.unicode_profile.kuromoji_dictionary_mut() {
                *dictionary = exact_name(required(profile)?);
            }
            false
        }
        _ => false,
    };
    canonicalize_set(filter);
    Ok(expanded)
}

fn required(profile: Option<&ResolvedDictionary>) -> AnalysisResult<&ResolvedDictionary> {
    profile.ok_or(AnalysisError::Descriptor(
        "missing resolved Japanese filter resource",
    ))
}

fn exact_name(profile: &ResolvedDictionary) -> String {
    format!("sha256:{}", profile.sha256())
}

pub(crate) fn canonicalize(config: &mut Analyzer) {
    for filter in &mut config.token_filters {
        canonicalize_set(filter);
    }
}

fn canonicalize_set(filter: &mut TokenFilter) {
    let words = match filter {
        TokenFilter::KuromojiPartOfSpeech(config) => config.stop_tags.as_mut(),
        TokenFilter::KuromojiStop(config) => config.words.as_mut(),
        _ => None,
    };
    if let Some(words) = words {
        words.sort();
        words.dedup();
    }
}

pub(super) fn check_resolved(filter: &TokenFilter) -> AnalysisResult<()> {
    match filter {
        TokenFilter::KuromojiPartOfSpeech(config) if config.stop_tags.is_none() => {
            return Err(AnalysisError::Descriptor(
                "resolved Japanese POS filters require explicit stop tags",
            ))
        }
        TokenFilter::KuromojiStop(config) if config.words.is_none() => {
            return Err(AnalysisError::Descriptor(
                "resolved Japanese stop filters require explicit words",
            ))
        }
        TokenFilter::KuromojiStop(config) if config.ignore_case && config.dictionary.is_none() => {
            return Err(AnalysisError::Descriptor(
                "resolved Japanese case-insensitive stops require an exact profile",
            ))
        }
        _ => {}
    }
    if let Some(dictionary) = dictionary_name(filter)? {
        super::exact(dictionary)?;
    }
    Ok(())
}
