//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original configuration images reconstruct native stages without resolving resources.

mod input;
mod morphology;

use input::{invalid, Input, Result};
use uqa_core::{
    json::JsonToken,
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

use crate::{Analyzer, CharFilter, NormalizationConfig, TokenFilter, Tokenizer};

pub(super) fn decode(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Analyzer>> {
    let mut input = Input::new(bytes, memory, cancellation)?;
    input.expect(JsonToken::StartObject)?;
    input.key("tokenizer")?;
    let tokenizer = input.tokenizer()?;
    input.key("token_filters")?;
    let token_filters = input.sequence(Input::token_filter)?;
    input.key("char_filters")?;
    let char_filters = input.sequence(Input::char_filter)?;
    let normalization = if input.has_key("normalization")? {
        input.key("normalization")?;
        Some(input.normalization()?)
    } else {
        None
    };
    input.expect(JsonToken::EndObject)?;
    input.finish(Analyzer {
        tokenizer,
        token_filters,
        char_filters,
        normalization,
    })
}

impl<'a> Input<'a, '_> {
    fn stage(&mut self) -> Result<&'a str> {
        self.expect(JsonToken::StartObject)?;
        self.key("type")?;
        self.tag()
    }

    fn tokenizer(&mut self) -> Result<Tokenizer> {
        let value = match self.stage()? {
            "whitespace" => Tokenizer::Whitespace,
            "standard" => Tokenizer::Standard,
            "letter" => Tokenizer::Letter,
            "keyword" => Tokenizer::Keyword,
            "n_gram" => {
                self.key("min_gram")?;
                let min_gram = self.number()?;
                self.key("max_gram")?;
                Tokenizer::NGram {
                    min_gram,
                    max_gram: self.number()?,
                }
            }
            "pattern" => {
                self.key("pattern")?;
                Tokenizer::Pattern {
                    pattern: self.text()?,
                }
            }
            #[cfg(feature = "nori")]
            "nori_tokenizer" => Tokenizer::Nori(self.nori_tokenizer()?),
            #[cfg(feature = "kuromoji")]
            "kuromoji_tokenizer" => Tokenizer::Kuromoji(self.kuromoji_tokenizer()?),
            _ => return Err(invalid()),
        };
        self.expect(JsonToken::EndObject)?;
        Ok(value)
    }

    fn token_filter(&mut self) -> Result<TokenFilter> {
        let value = match self.stage()? {
            "lowercase" => TokenFilter::Lowercase,
            "porter_stem" => TokenFilter::PorterStem,
            "ascii_folding" => TokenFilter::ASCIIFolding,
            "stop" => {
                self.key("language")?;
                let language = self.text()?;
                self.key("custom_words")?;
                TokenFilter::Stop {
                    language,
                    custom_words: self.strings()?,
                }
            }
            "synonym" => {
                let synonyms = if self.has_key("synonyms")? {
                    self.key("synonyms")?;
                    self.map(Self::strings)?
                } else {
                    std::collections::BTreeMap::new()
                };
                let synonyms_path = if self.has_key("synonyms_path")? {
                    self.key("synonyms_path")?;
                    Some(self.text()?.into())
                } else {
                    None
                };
                TokenFilter::Synonym {
                    synonyms,
                    synonyms_path,
                }
            }
            "ngram" => {
                self.key("min_gram")?;
                let min_gram = self.number()?;
                self.key("max_gram")?;
                let max_gram = self.number()?;
                self.key("keep_short")?;
                TokenFilter::Ngram {
                    min_gram,
                    max_gram,
                    keep_short: self.boolean()?,
                }
            }
            "edge_ngram" => {
                self.key("min_gram")?;
                let min_gram = self.number()?;
                self.key("max_gram")?;
                TokenFilter::EdgeNgram {
                    min_gram,
                    max_gram: self.number()?,
                }
            }
            "length" => {
                self.key("min_length")?;
                let min_length = self.number()?;
                self.key("max_length")?;
                TokenFilter::Length {
                    min_length,
                    max_length: self.number()?,
                }
            }
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            tag => self.morphology_filter(tag)?,
            #[cfg(not(any(feature = "nori", feature = "kuromoji")))]
            _ => return Err(invalid()),
        };
        self.expect(JsonToken::EndObject)?;
        Ok(value)
    }

    fn char_filter(&mut self) -> Result<CharFilter> {
        let value = match self.stage()? {
            "html_strip" => CharFilter::HTMLStrip,
            "cjk_width" => CharFilter::CJKWidth,
            "mapping" => {
                self.key("mapping")?;
                CharFilter::Mapping {
                    mapping: self.map(Self::text)?,
                }
            }
            "pattern_replace" => {
                self.key("pattern")?;
                let pattern = self.text()?;
                self.key("replacement")?;
                CharFilter::PatternReplace {
                    pattern,
                    replacement: self.text()?,
                }
            }
            #[cfg(feature = "kuromoji")]
            "kuromoji_iteration_mark" => {
                self.key("normalize_kanji")?;
                let normalize_kanji = self.boolean()?;
                self.key("normalize_kana")?;
                CharFilter::KuromojiIterationMark {
                    normalize_kanji,
                    normalize_kana: self.boolean()?,
                }
            }
            _ => return Err(invalid()),
        };
        self.expect(JsonToken::EndObject)?;
        Ok(value)
    }

    fn normalization(&mut self) -> Result<NormalizationConfig> {
        let value = match self.stage()? {
            "unavailable" => NormalizationConfig::Unavailable,
            "cjk_width" => NormalizationConfig::CJKWidth,
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            "unicode_simple_lowercase" => {
                self.key("profile")?;
                NormalizationConfig::UnicodeSimpleLowercase {
                    profile: self.profile()?,
                }
            }
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            "cjk_width_simple_lowercase" => {
                self.key("profile")?;
                NormalizationConfig::CJKWidthSimpleLowercase {
                    profile: self.profile()?,
                }
            }
            _ => return Err(invalid()),
        };
        self.expect(JsonToken::EndObject)?;
        Ok(value)
    }
}
