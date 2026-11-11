//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Feature-selected native stages retain exact language defaults without resource resolution.

#[cfg(any(feature = "nori", feature = "kuromoji"))]
use super::Item;
#[cfg(any(feature = "nori", feature = "kuromoji"))]
use super::Read;
#[cfg(any(feature = "nori", feature = "kuromoji"))]
use crate::TokenFilter;
#[cfg(any(feature = "nori", feature = "kuromoji"))]
use serde::de::Error as _;

pub(super) const FILTER_TAGS: &[&str] = &[
    #[cfg(feature = "kuromoji")]
    "kuromoji_part_of_speech",
    #[cfg(feature = "kuromoji")]
    "kuromoji_stop",
    #[cfg(feature = "kuromoji")]
    "kuromoji_completion",
    #[cfg(feature = "kuromoji")]
    "kuromoji_baseform",
    #[cfg(feature = "kuromoji")]
    "kuromoji_stemmer",
    #[cfg(feature = "kuromoji")]
    "kuromoji_hiragana_uppercase",
    #[cfg(feature = "kuromoji")]
    "kuromoji_katakana_uppercase",
    #[cfg(feature = "kuromoji")]
    "kuromoji_readingform",
    #[cfg(feature = "kuromoji")]
    "kuromoji_number",
    #[cfg(feature = "nori")]
    "nori_part_of_speech",
    #[cfg(feature = "nori")]
    "nori_readingform",
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    "unicode_simple_lowercase",
    #[cfg(feature = "nori")]
    "nori_number",
    "lowercase",
    "stop",
    "porter_stem",
    "a_s_c_i_i_folding",
    "ascii_folding",
    "synonym",
    "ngram",
    "edge_ngram",
    "length",
];

#[cfg(feature = "nori")]
pub(super) async fn nori<R: Read>(
    input: &mut R,
) -> Result<crate::nori::NoriTokenizerConfig, R::Error> {
    use crate::nori::{DecompoundMode, NoriTokenizerConfig};
    let (
        dictionary,
        decompound_mode,
        output_unknown_unigrams,
        discard_punctuation,
        user_dictionary,
    ) = fields!(input, true;
        dictionary: String => default(NoriTokenizerConfig::default().dictionary),
        decompound_mode: DecompoundMode => default(NoriTokenizerConfig::default().decompound_mode),
        output_unknown_unigrams: bool => default(NoriTokenizerConfig::default().output_unknown_unigrams),
        discard_punctuation: bool => default(NoriTokenizerConfig::default().discard_punctuation),
        user_dictionary: Option<String> => default(NoriTokenizerConfig::default().user_dictionary),
    );
    Ok(NoriTokenizerConfig {
        dictionary,
        decompound_mode,
        output_unknown_unigrams,
        discard_punctuation,
        user_dictionary,
    })
}

#[cfg(feature = "kuromoji")]
pub(super) async fn kuromoji<R: Read>(
    input: &mut R,
) -> Result<crate::kuromoji::KuromojiTokenizerConfig, R::Error> {
    use crate::kuromoji::{KuromojiMode, KuromojiTokenizerConfig};
    let (
        dictionary,
        mode,
        discard_punctuation,
        discard_compound_token,
        user_dictionary,
        n_best_cost,
        n_best_examples,
    ) = fields!(input, true;
        dictionary: String => default(KuromojiTokenizerConfig::default().dictionary),
        mode: KuromojiMode => default(KuromojiTokenizerConfig::default().mode),
        discard_punctuation: bool => default(KuromojiTokenizerConfig::default().discard_punctuation),
        discard_compound_token: bool => default(KuromojiTokenizerConfig::default().discard_compound_token),
        user_dictionary: Option<String> => default(KuromojiTokenizerConfig::default().user_dictionary),
        n_best_cost: i32 => default(KuromojiTokenizerConfig::default().n_best_cost),
        n_best_examples: Option<String> => default(KuromojiTokenizerConfig::default().n_best_examples),
    );
    Ok(KuromojiTokenizerConfig {
        dictionary,
        mode,
        discard_punctuation,
        discard_compound_token,
        user_dictionary,
        n_best_cost,
        n_best_examples,
    })
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
pub(super) async fn filter<R: Read>(input: &mut R, tag: &str) -> Result<TokenFilter, R::Error> {
    Ok(match tag {
        #[cfg(feature = "nori")]
        "nori_part_of_speech" => {
            let (stop_tags,) =
                fields!(input, true; stop_tags: Option<Vec<crate::nori::POSTag>> => default(None));
            TokenFilter::NoriPartOfSpeech(crate::nori::NoriPOSConfig { stop_tags })
        }
        #[cfg(feature = "nori")]
        "nori_readingform" => {
            fields!(input, true;);
            TokenFilter::NoriReadingForm(crate::EmptyFilterConfig {})
        }
        #[cfg(feature = "nori")]
        "nori_number" => {
            fields!(input, true;);
            TokenFilter::NoriNumber(crate::EmptyFilterConfig {})
        }
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        "unicode_simple_lowercase" => {
            let (unicode_profile,) = fields!(input, true; unicode_profile: crate::UnicodeProfileSource => default(crate::UnicodeProfileSource::default()));
            TokenFilter::UnicodeSimpleLowercase(crate::SimpleLowercaseConfig { unicode_profile })
        }
        #[cfg(feature = "kuromoji")]
        "kuromoji_part_of_speech" => {
            let (stop_tags, dictionary) = fields!(input, true;
                stop_tags: Option<Vec<String>> => default(None), dictionary: Option<String> => default(None),
            );
            TokenFilter::KuromojiPartOfSpeech(crate::kuromoji::KuromojiPOSConfig {
                stop_tags,
                dictionary,
            })
        }
        #[cfg(feature = "kuromoji")]
        "kuromoji_stop" => {
            let (words, ignore_case, dictionary) = fields!(input, true;
                words: Option<Vec<String>> => default(crate::kuromoji::KuromojiStopConfig::default().words),
                ignore_case: bool => default(crate::kuromoji::KuromojiStopConfig::default().ignore_case),
                dictionary: Option<String> => default(crate::kuromoji::KuromojiStopConfig::default().dictionary),
            );
            TokenFilter::KuromojiStop(crate::kuromoji::KuromojiStopConfig {
                words,
                ignore_case,
                dictionary,
            })
        }
        #[cfg(feature = "kuromoji")]
        "kuromoji_completion" => {
            let (dictionary, mode) = fields!(input, true;
                dictionary: String => default(crate::kuromoji::KuromojiCompletionConfig::default().dictionary),
                mode: crate::kuromoji::CompletionMode => default(crate::kuromoji::KuromojiCompletionConfig::default().mode),
            );
            TokenFilter::KuromojiCompletion(crate::kuromoji::KuromojiCompletionConfig {
                dictionary,
                mode,
            })
        }
        #[cfg(feature = "kuromoji")]
        "kuromoji_stemmer" => {
            let (minimum_length,) = fields!(input, true;
                minimum_length: i32 => default(crate::kuromoji::KuromojiStemConfig::default().minimum_length),
            );
            TokenFilter::KuromojiStem(crate::kuromoji::KuromojiStemConfig { minimum_length })
        }
        #[cfg(feature = "kuromoji")]
        "kuromoji_readingform" => {
            let (use_romaji,) = fields!(input, true; use_romaji: bool => default(false));
            TokenFilter::KuromojiReadingForm(crate::kuromoji::KuromojiReadingFormConfig {
                use_romaji,
            })
        }
        #[cfg(feature = "kuromoji")]
        "kuromoji_baseform" => {
            fields!(input, true;);
            TokenFilter::KuromojiBaseForm(crate::EmptyFilterConfig {})
        }
        #[cfg(feature = "kuromoji")]
        "kuromoji_hiragana_uppercase" => {
            fields!(input, true;);
            TokenFilter::KuromojiHiraganaUppercase(crate::EmptyFilterConfig {})
        }
        #[cfg(feature = "kuromoji")]
        "kuromoji_katakana_uppercase" => {
            fields!(input, true;);
            TokenFilter::KuromojiKatakanaUppercase(crate::EmptyFilterConfig {})
        }
        #[cfg(feature = "kuromoji")]
        "kuromoji_number" => {
            fields!(input, true;);
            TokenFilter::KuromojiNumber(crate::EmptyFilterConfig {})
        }
        _ => return Err(input.error(serde_json::Error::unknown_variant(tag, FILTER_TAGS))),
    })
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
impl Item for crate::UnicodeProfile {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        const TAGS: &[&str] = &[
            #[cfg(feature = "nori")]
            "nori",
            #[cfg(feature = "kuromoji")]
            "kuromoji",
        ];
        let tag = input.tagged("UnicodeProfile", "provider", TAGS).await?;
        match tag {
            #[cfg(feature = "nori")]
            "nori" => {
                let (dictionary,) = fields!(input, true, "struct variant UnicodeProfile::Nori"; dictionary: String => required);
                Ok(Self::Nori { dictionary })
            }
            #[cfg(feature = "kuromoji")]
            "kuromoji" => {
                let (dictionary,) = fields!(input, true, "struct variant UnicodeProfile::Kuromoji"; dictionary: String => required);
                Ok(Self::Kuromoji { dictionary })
            }
            _ => Err(input.error(serde_json::Error::unknown_variant(tag, TAGS))),
        }
    }
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
impl Item for crate::UnicodeProfileSource {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        if input.is_text().await? {
            return input.text("a string").await.map(Self::LegacyNori);
        }
        crate::UnicodeProfile::read(input)
            .await
            .map(Self::Explicit)
            .map_err(|error| input.untagged(error, "UnicodeProfileSource"))
    }
}
