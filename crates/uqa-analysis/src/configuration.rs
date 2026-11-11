//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native configuration constructors share their vocabulary across original images and descriptors.

use serde::de::Error as _;

use crate::{Analyzer, CharFilter, NormalizationConfig, TokenFilter, Tokenizer};

mod input;
use input::Omitted;
pub(crate) use input::{Item, Read};

macro_rules! field_value {
    ($input:ident, $name:ident, required) => {
        $name.ok_or_else(|| $input.missing(stringify!($name)))?
    };
    ($input:ident, $name:ident, default($default:expr)) => {
        match $name {
            Some(value) => value,
            None if $input.implicit_defaults() => $default,
            None => return Err($input.missing(stringify!($name))),
        }
    };
    ($input:ident, $name:ident, optional($default:expr)) => {
        $name.unwrap_or_else(|| $default)
    };
}

macro_rules! field_omission {
    (optional) => {
        true
    };
    (required) => {
        false
    };
    (default) => {
        false
    };
}

macro_rules! fields {
    ($input:ident, $deny:expr;) => {{
        let mut previous = None;
        let _ = $input.field(&[], &[], "", $deny, &mut previous).await?;
    }};
    ($input:ident, $deny:expr; $($rest:tt)*) => { fields!($input, $deny, ""; $($rest)*) };
    ($input:ident, $deny:expr, $description:literal; $($name:ident: $ty:ty => $mode:ident $(($default:expr))?),* $(,)?) => {{
        const NAMES: &[&str] = &[$(stringify!($name)),*];
        const OMITTED: &[bool] = &[$(field_omission!($mode)),*];
        $(let mut $name: Option<$ty> = None;)*
        let mut previous = None;
        while let Some(index) = $input.field(NAMES, OMITTED, $description, $deny, &mut previous).await? {
            match NAMES[index] {
                $(stringify!($name) => $name = Some(<$ty as Item>::read($input).await?),)*
                _ => unreachable!("the source selects a declared configuration field"),
            }
        }
        ($(field_value!($input, $name, $mode $(($default))?),)*)
    }};
}

mod inspection;
mod morphology;
mod view;
pub(crate) use inspection::{check_encoded_size, matches_snapshot};
mod value;
pub(crate) use value::decode;

#[cfg(test)]
mod tests;

pub(crate) async fn analyzer<R: Read>(input: &mut R) -> Result<Analyzer, R::Error> {
    input.object("Analyzer").await?;
    let (tokenizer, token_filters, char_filters, normalization) = fields!(input, false;
        tokenizer: Tokenizer => default(Tokenizer::Whitespace),
        token_filters: Vec<TokenFilter> => default(Vec::new()),
        char_filters: Vec<CharFilter> => default(Vec::new()),
        normalization: Omitted<NormalizationConfig> => optional(Omitted(None)),
    );
    Ok(Analyzer {
        tokenizer,
        token_filters,
        char_filters,
        normalization: normalization.0,
    })
}

impl Item for Tokenizer {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        const TAGS: &[&str] = &[
            "whitespace",
            "standard",
            "letter",
            "n_gram",
            "pattern",
            "keyword",
            #[cfg(feature = "nori")]
            "nori_tokenizer",
            #[cfg(feature = "kuromoji")]
            "kuromoji_tokenizer",
        ];
        let tag = input.tagged("Tokenizer", "type", TAGS).await?;
        Ok(match tag {
            "whitespace" => {
                fields!(input, false;);
                Self::Whitespace
            }
            "standard" => {
                fields!(input, false;);
                Self::Standard
            }
            "letter" => {
                fields!(input, false;);
                Self::Letter
            }
            "keyword" => {
                fields!(input, false;);
                Self::Keyword
            }
            "n_gram" => {
                let (min_gram, max_gram) = fields!(input, false, "struct variant Tokenizer::NGram";
                    min_gram: usize => required, max_gram: usize => required,
                );
                Self::NGram { min_gram, max_gram }
            }
            "pattern" => {
                let (pattern,) = fields!(input, false, "struct variant Tokenizer::Pattern"; pattern: String => required);
                Self::Pattern { pattern }
            }
            #[cfg(feature = "nori")]
            "nori_tokenizer" => Self::Nori(morphology::nori(input).await?),
            #[cfg(feature = "kuromoji")]
            "kuromoji_tokenizer" => Self::Kuromoji(morphology::kuromoji(input).await?),
            _ => return Err(input.error(serde_json::Error::unknown_variant(tag, TAGS))),
        })
    }
}

impl Item for TokenFilter {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        let tag = input
            .tagged("TokenFilter", "type", morphology::FILTER_TAGS)
            .await?;
        Ok(match tag {
            "lowercase" => {
                fields!(input, false;);
                Self::Lowercase
            }
            "porter_stem" => {
                fields!(input, false;);
                Self::PorterStem
            }
            "ascii_folding" => {
                fields!(input, false;);
                Self::ASCIIFolding
            }
            "a_s_c_i_i_folding" if input.implicit_defaults() => {
                fields!(input, false;);
                Self::ASCIIFolding
            }
            "stop" => {
                let (language, custom_words) = fields!(input, false;
                    language: String => default(crate::token_filter::default_stop_language()),
                    custom_words: Vec<String> => default(Vec::new()),
                );
                Self::Stop {
                    language,
                    custom_words,
                }
            }
            "synonym" => {
                let (synonyms, synonyms_path) = fields!(input, false;
                    synonyms: std::collections::BTreeMap<String, Vec<String>> => optional(std::collections::BTreeMap::new()),
                    synonyms_path: Omitted<std::path::PathBuf> => optional(Omitted(None)),
                );
                Self::Synonym {
                    synonyms,
                    synonyms_path: synonyms_path.0,
                }
            }
            "ngram" => {
                let (min_gram, max_gram, keep_short) = fields!(input, false, "struct variant TokenFilter::Ngram";
                    min_gram: usize => required, max_gram: usize => required,
                    keep_short: bool => default(false),
                );
                Self::Ngram {
                    min_gram,
                    max_gram,
                    keep_short,
                }
            }
            "edge_ngram" => {
                let (min_gram, max_gram) = fields!(input, false, "struct variant TokenFilter::EdgeNgram";
                    min_gram: usize => required, max_gram: usize => required,
                );
                Self::EdgeNgram { min_gram, max_gram }
            }
            "length" => {
                let (min_length, max_length) = fields!(input, false;
                    min_length: usize => default(0), max_length: usize => default(0),
                );
                Self::Length {
                    min_length,
                    max_length,
                }
            }
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            _ => morphology::filter(input, tag).await?,
            #[cfg(not(any(feature = "nori", feature = "kuromoji")))]
            _ => {
                return Err(input.error(serde_json::Error::unknown_variant(
                    tag,
                    morphology::FILTER_TAGS,
                )))
            }
        })
    }
}

impl Item for CharFilter {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        const TAGS: &[&str] = &[
            "h_t_m_l_strip",
            "html_strip",
            "cjk_width",
            #[cfg(feature = "kuromoji")]
            "kuromoji_iteration_mark",
            "mapping",
            "pattern_replace",
        ];
        let tag = input.tagged("CharFilter", "type", TAGS).await?;
        Ok(match tag {
            "html_strip" => {
                fields!(input, false;);
                Self::HTMLStrip
            }
            "h_t_m_l_strip" if input.implicit_defaults() => {
                fields!(input, false;);
                Self::HTMLStrip
            }
            "cjk_width" => {
                fields!(input, false;);
                Self::CJKWidth
            }
            "mapping" => {
                let (mapping,) = fields!(input, false, "struct variant CharFilter::Mapping";
                    mapping: std::collections::BTreeMap<String, String> => required,
                );
                Self::Mapping { mapping }
            }
            "pattern_replace" => {
                let (pattern, replacement) = fields!(input, false, "struct variant CharFilter::PatternReplace";
                    pattern: String => required, replacement: String => default(String::new()),
                );
                Self::PatternReplace {
                    pattern,
                    replacement,
                }
            }
            #[cfg(feature = "kuromoji")]
            "kuromoji_iteration_mark" => {
                let (normalize_kanji, normalize_kana) = fields!(input, false;
                    normalize_kanji: bool => default(true), normalize_kana: bool => default(true),
                );
                Self::KuromojiIterationMark {
                    normalize_kanji,
                    normalize_kana,
                }
            }
            _ => return Err(input.error(serde_json::Error::unknown_variant(tag, TAGS))),
        })
    }
}

impl Item for NormalizationConfig {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        const TAGS: &[&str] = &[
            "unavailable",
            "cjk_width",
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            "unicode_simple_lowercase",
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            "cjk_width_simple_lowercase",
        ];
        let tag = input.tagged("NormalizationConfig", "type", TAGS).await?;
        Ok(match tag {
            "unavailable" => {
                fields!(input, true;);
                Self::Unavailable
            }
            "cjk_width" => {
                fields!(input, true;);
                Self::CJKWidth
            }
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            "unicode_simple_lowercase" | "cjk_width_simple_lowercase" => {
                let (profile,) = if tag == "unicode_simple_lowercase" {
                    fields!(input, true, "struct variant NormalizationConfig::UnicodeSimpleLowercase"; profile: crate::UnicodeProfile => required)
                } else {
                    fields!(input, true, "struct variant NormalizationConfig::CJKWidthSimpleLowercase"; profile: crate::UnicodeProfile => required)
                };
                if tag == "unicode_simple_lowercase" {
                    Self::UnicodeSimpleLowercase { profile }
                } else {
                    Self::CJKWidthSimpleLowercase { profile }
                }
            }
            _ => return Err(input.error(serde_json::Error::unknown_variant(tag, TAGS))),
        })
    }
}
