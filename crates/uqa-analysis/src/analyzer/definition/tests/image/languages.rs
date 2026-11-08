//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

#[cfg(any(feature = "nori", feature = "kuromoji"))]
use super::*;

#[cfg(feature = "nori")]
#[test]
fn original_analyzer_input_preserves_unresolved_korean_options() {
    use crate::nori::{DecompoundMode, NoriPOSConfig, NoriTokenizerConfig, POSTag};
    let filters = vec![
        TokenFilter::NoriPartOfSpeech(NoriPOSConfig { stop_tags: None }),
        TokenFilter::NoriPartOfSpeech(NoriPOSConfig {
            stop_tags: Some(Vec::new()),
        }),
        TokenFilter::NoriPartOfSpeech(NoriPOSConfig {
            stop_tags: Some(
                POSTag::NAMES
                    .iter()
                    .rev()
                    .map(|name| POSTag::from_name(name).unwrap())
                    .collect(),
            ),
        }),
        TokenFilter::NoriReadingForm(crate::EmptyFilterConfig {}),
        TokenFilter::NoriNumber(crate::EmptyFilterConfig {}),
        TokenFilter::UnicodeSimpleLowercase(crate::SimpleLowercaseConfig {
            unicode_profile: crate::UnicodeProfileSource::LegacyNori("not-resolved".into()),
        }),
        TokenFilter::UnicodeSimpleLowercase(crate::SimpleLowercaseConfig {
            unicode_profile: crate::UnicodeProfile::Nori {
                dictionary: String::new(),
            }
            .into(),
        }),
    ];
    for decompound_mode in [
        DecompoundMode::None,
        DecompoundMode::Discard,
        DecompoundMode::Mixed,
    ] {
        for user_dictionary in [
            None,
            Some(String::new()),
            Some("invalid-user-dictionary".into()),
        ] {
            restored(&Analyzer {
                tokenizer: Tokenizer::Nori(NoriTokenizerConfig {
                    dictionary: "not-resolved".into(),
                    decompound_mode,
                    output_unknown_unigrams: true,
                    discard_punctuation: false,
                    user_dictionary,
                }),
                token_filters: filters.clone(),
                char_filters: Vec::new(),
                normalization: Some(NormalizationConfig::UnicodeSimpleLowercase {
                    profile: crate::UnicodeProfile::Nori {
                        dictionary: "not-resolved".into(),
                    },
                }),
            });
        }
    }
    restored(&crate::nori::nori_analyzer());
}

#[cfg(feature = "kuromoji")]
#[test]
fn original_analyzer_input_preserves_unresolved_japanese_options() {
    use crate::{
        kuromoji::*, EmptyFilterConfig, SimpleLowercaseConfig, UnicodeProfile, UnicodeProfileSource,
    };
    let filters = vec![
        TokenFilter::KuromojiPartOfSpeech(KuromojiPOSConfig {
            stop_tags: None,
            dictionary: None,
        }),
        TokenFilter::KuromojiPartOfSpeech(KuromojiPOSConfig {
            stop_tags: Some(vec!["名詞".into(), String::new(), "名詞".into()]),
            dictionary: Some(String::new()),
        }),
        TokenFilter::KuromojiStop(KuromojiStopConfig {
            words: None,
            ignore_case: true,
            dictionary: None,
        }),
        TokenFilter::KuromojiStop(KuromojiStopConfig {
            words: Some(Vec::new()),
            ignore_case: false,
            dictionary: Some("unavailable".into()),
        }),
        TokenFilter::KuromojiCompletion(KuromojiCompletionConfig {
            dictionary: String::new(),
            mode: CompletionMode::Index,
        }),
        TokenFilter::KuromojiCompletion(KuromojiCompletionConfig {
            dictionary: "unavailable".into(),
            mode: CompletionMode::Query,
        }),
        TokenFilter::KuromojiBaseForm(EmptyFilterConfig {}),
        TokenFilter::KuromojiStem(KuromojiStemConfig {
            minimum_length: i32::MIN,
        }),
        TokenFilter::KuromojiHiraganaUppercase(EmptyFilterConfig {}),
        TokenFilter::KuromojiKatakanaUppercase(EmptyFilterConfig {}),
        TokenFilter::KuromojiReadingForm(KuromojiReadingFormConfig { use_romaji: true }),
        TokenFilter::KuromojiNumber(EmptyFilterConfig {}),
        TokenFilter::UnicodeSimpleLowercase(SimpleLowercaseConfig {
            unicode_profile: UnicodeProfile::Kuromoji {
                dictionary: "unavailable".into(),
            }
            .into(),
        }),
        TokenFilter::UnicodeSimpleLowercase(SimpleLowercaseConfig {
            unicode_profile: UnicodeProfileSource::LegacyNori("jdk21".into()),
        }),
    ];
    for mode in [
        KuromojiMode::Normal,
        KuromojiMode::Search,
        KuromojiMode::Extended,
    ] {
        for user_dictionary in [
            None,
            Some(String::new()),
            Some("invalid-user-dictionary".into()),
        ] {
            restored(&Analyzer {
                tokenizer: Tokenizer::Kuromoji(KuromojiTokenizerConfig {
                    dictionary: "unavailable".into(),
                    mode,
                    discard_punctuation: false,
                    discard_compound_token: true,
                    user_dictionary,
                    n_best_cost: i32::MIN,
                    n_best_examples: Some("unresolved-probe".into()),
                }),
                token_filters: filters.clone(),
                char_filters: vec![CharFilter::KuromojiIterationMark {
                    normalize_kanji: false,
                    normalize_kana: true,
                }],
                normalization: Some(NormalizationConfig::CJKWidthSimpleLowercase {
                    profile: UnicodeProfile::Kuromoji {
                        dictionary: "unavailable".into(),
                    },
                }),
            });
        }
    }
    restored(&Analyzer {
        tokenizer: Tokenizer::Kuromoji(KuromojiTokenizerConfig::default()),
        ..Analyzer::default()
    });
}
