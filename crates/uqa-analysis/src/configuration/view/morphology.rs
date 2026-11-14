//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Language configuration views preserve the public field order and optional values.

#[cfg(any(feature = "nori", feature = "kuromoji"))]
use super::{fields, optional_text, scalar, tagged, Fields, Scalar, View};
#[cfg(any(feature = "nori", feature = "kuromoji"))]
use crate::TokenFilter;

#[cfg(feature = "nori")]
pub(super) fn nori(value: &crate::nori::NoriTokenizerConfig) -> Fields<'_> {
    tagged(
        "nori_tokenizer",
        [
            ("dictionary", View::Text(&value.dictionary)),
            (
                "decompound_mode",
                View::Scalar(Scalar::DecompoundMode(value.decompound_mode)),
            ),
            (
                "output_unknown_unigrams",
                scalar(value.output_unknown_unigrams),
            ),
            ("discard_punctuation", scalar(value.discard_punctuation)),
            (
                "user_dictionary",
                optional_text(value.user_dictionary.as_deref()),
            ),
        ],
    )
}

#[cfg(feature = "kuromoji")]
pub(super) fn kuromoji(value: &crate::kuromoji::KuromojiTokenizerConfig) -> Fields<'_> {
    tagged(
        "kuromoji_tokenizer",
        [
            ("dictionary", View::Text(&value.dictionary)),
            ("mode", View::Scalar(Scalar::KuromojiMode(value.mode))),
            ("discard_punctuation", scalar(value.discard_punctuation)),
            (
                "discard_compound_token",
                scalar(value.discard_compound_token),
            ),
            (
                "user_dictionary",
                optional_text(value.user_dictionary.as_deref()),
            ),
            ("n_best_cost", scalar(value.n_best_cost)),
            (
                "n_best_examples",
                optional_text(value.n_best_examples.as_deref()),
            ),
        ],
    )
}

#[cfg(feature = "kuromoji")]
fn optional_strings(value: Option<&[String]>) -> View<'_> {
    value.map_or(View::Scalar(Scalar::Null), View::Strings)
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
pub(super) fn filter(value: &TokenFilter) -> Fields<'_> {
    match value {
        #[cfg(feature = "nori")]
        TokenFilter::NoriPartOfSpeech(config) => tagged(
            "nori_part_of_speech",
            [(
                "stop_tags",
                config
                    .stop_tags
                    .as_deref()
                    .map_or(View::Scalar(Scalar::Null), View::POSTags),
            )],
        ),
        #[cfg(feature = "nori")]
        TokenFilter::NoriReadingForm(_) => tagged("nori_readingform", []),
        #[cfg(feature = "nori")]
        TokenFilter::NoriNumber(_) => tagged("nori_number", []),
        TokenFilter::UnicodeSimpleLowercase(config) => tagged(
            "unicode_simple_lowercase",
            [(
                "unicode_profile",
                match &config.unicode_profile {
                    crate::UnicodeProfileSource::LegacyNori(value) => View::Text(value),
                    crate::UnicodeProfileSource::Explicit(value) => View::Profile(value),
                },
            )],
        ),
        #[cfg(feature = "kuromoji")]
        TokenFilter::KuromojiPartOfSpeech(config) => tagged(
            "kuromoji_part_of_speech",
            [
                ("stop_tags", optional_strings(config.stop_tags.as_deref())),
                ("dictionary", optional_text(config.dictionary.as_deref())),
            ],
        ),
        #[cfg(feature = "kuromoji")]
        TokenFilter::KuromojiStop(config) => tagged(
            "kuromoji_stop",
            [
                ("words", optional_strings(config.words.as_deref())),
                ("ignore_case", scalar(config.ignore_case)),
                ("dictionary", optional_text(config.dictionary.as_deref())),
            ],
        ),
        #[cfg(feature = "kuromoji")]
        TokenFilter::KuromojiCompletion(config) => tagged(
            "kuromoji_completion",
            [
                ("dictionary", View::Text(&config.dictionary)),
                ("mode", View::Scalar(Scalar::CompletionMode(config.mode))),
            ],
        ),
        #[cfg(feature = "kuromoji")]
        TokenFilter::KuromojiBaseForm(_) => tagged("kuromoji_baseform", []),
        #[cfg(feature = "kuromoji")]
        TokenFilter::KuromojiStem(config) => tagged(
            "kuromoji_stemmer",
            [("minimum_length", scalar(config.minimum_length))],
        ),
        #[cfg(feature = "kuromoji")]
        TokenFilter::KuromojiHiraganaUppercase(_) => tagged("kuromoji_hiragana_uppercase", []),
        #[cfg(feature = "kuromoji")]
        TokenFilter::KuromojiKatakanaUppercase(_) => tagged("kuromoji_katakana_uppercase", []),
        #[cfg(feature = "kuromoji")]
        TokenFilter::KuromojiReadingForm(config) => tagged(
            "kuromoji_readingform",
            [("use_romaji", scalar(config.use_romaji))],
        ),
        #[cfg(feature = "kuromoji")]
        TokenFilter::KuromojiNumber(_) => tagged("kuromoji_number", []),
        _ => unreachable!("common stages are selected before language configuration views"),
    }
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
pub(super) fn profile(value: &crate::UnicodeProfile) -> Fields<'_> {
    let (provider, dictionary) = match value {
        #[cfg(feature = "nori")]
        crate::UnicodeProfile::Nori { dictionary } => ("nori", dictionary),
        #[cfg(feature = "kuromoji")]
        crate::UnicodeProfile::Kuromoji { dictionary } => ("kuromoji", dictionary),
    };
    fields([
        ("provider", View::Text(provider)),
        ("dictionary", View::Text(dictionary)),
    ])
}
