//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow native configuration fields without materializing another complete pipeline.

use std::collections::{btree_map, BTreeMap};

use serde_json::Value;

use crate::{AnalysisResult, Analyzer, CharFilter, NormalizationConfig, TokenFilter, Tokenizer};

mod morphology;

pub(super) enum View<'a> {
    Analyzer(&'a Analyzer),
    Tokenizer(&'a Tokenizer),
    TokenFilter(&'a TokenFilter),
    CharFilter(&'a CharFilter),
    Normalization(&'a NormalizationConfig),
    TokenFilters(&'a [TokenFilter]),
    CharFilters(&'a [CharFilter]),
    Strings(&'a [String]),
    Mapping(&'a BTreeMap<String, String>),
    Synonyms(&'a BTreeMap<String, Vec<String>>),
    Text(&'a str),
    Scalar(Value),
    #[cfg(feature = "nori")]
    POSTags(&'a [crate::nori::POSTag]),
    #[cfg(feature = "nori")]
    POSTag(&'a crate::nori::POSTag),
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    Profile(&'a crate::UnicodeProfile),
}

pub(super) enum Form<'a> {
    Object(Object<'a>),
    Array(Array<'a>),
    Text(&'a str),
    Scalar(Value),
}

pub(super) enum Object<'a> {
    Fields(std::vec::IntoIter<(&'a str, View<'a>)>),
    Mapping(btree_map::Iter<'a, String, String>),
    Synonyms(btree_map::Iter<'a, String, Vec<String>>),
}

impl<'a> Iterator for Object<'a> {
    type Item = (&'a str, View<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Fields(values) => values.next(),
            Self::Mapping(values) => values
                .next()
                .map(|(key, value)| (key.as_str(), View::Text(value))),
            Self::Synonyms(values) => values
                .next()
                .map(|(key, value)| (key.as_str(), View::Strings(value))),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let length = match self {
            Self::Fields(values) => values.len(),
            Self::Mapping(values) => values.len(),
            Self::Synonyms(values) => values.len(),
        };
        (length, Some(length))
    }
}

impl ExactSizeIterator for Object<'_> {}

pub(super) enum Array<'a> {
    TokenFilters(std::slice::Iter<'a, TokenFilter>),
    CharFilters(std::slice::Iter<'a, CharFilter>),
    Strings(std::slice::Iter<'a, String>),
    #[cfg(feature = "nori")]
    POSTags(std::slice::Iter<'a, crate::nori::POSTag>),
}

impl<'a> Iterator for Array<'a> {
    type Item = View<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::TokenFilters(values) => values.next().map(View::TokenFilter),
            Self::CharFilters(values) => values.next().map(View::CharFilter),
            Self::Strings(values) => values.next().map(|value| View::Text(value)),
            #[cfg(feature = "nori")]
            Self::POSTags(values) => values.next().map(View::POSTag),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let length = match self {
            Self::TokenFilters(values) => values.len(),
            Self::CharFilters(values) => values.len(),
            Self::Strings(values) => values.len(),
            #[cfg(feature = "nori")]
            Self::POSTags(values) => values.len(),
        };
        (length, Some(length))
    }
}

impl ExactSizeIterator for Array<'_> {}

fn scalar<'a>(value: impl Into<Value>) -> View<'a> {
    View::Scalar(value.into())
}

fn optional_text(value: Option<&str>) -> View<'_> {
    value.map_or(View::Scalar(Value::Null), View::Text)
}

fn fields<'a>(values: Vec<(&'a str, View<'a>)>) -> Form<'a> {
    Form::Object(Object::Fields(values.into_iter()))
}

fn tagged<'a>(tag: &'static str, mut values: Vec<(&'a str, View<'a>)>) -> Form<'a> {
    values.insert(0, ("type", View::Text(tag)));
    fields(values)
}

impl<'a> View<'a> {
    /// Snapshot comparison includes the two explicit synonym fields omitted by ordinary serialization.
    pub(super) fn form(self, snapshot: bool) -> AnalysisResult<Form<'a>> {
        Ok(match self {
            Self::Analyzer(config) => {
                let mut values = vec![
                    ("tokenizer", Self::Tokenizer(&config.tokenizer)),
                    ("token_filters", Self::TokenFilters(&config.token_filters)),
                    ("char_filters", Self::CharFilters(&config.char_filters)),
                ];
                if let Some(value) = &config.normalization {
                    values.push(("normalization", Self::Normalization(value)));
                }
                fields(values)
            }
            Self::Tokenizer(config) => match config {
                Tokenizer::Whitespace => tagged("whitespace", vec![]),
                Tokenizer::Standard => tagged("standard", vec![]),
                Tokenizer::Letter => tagged("letter", vec![]),
                Tokenizer::Keyword => tagged("keyword", vec![]),
                Tokenizer::NGram { min_gram, max_gram } => tagged(
                    "n_gram",
                    vec![
                        ("min_gram", scalar(*min_gram)),
                        ("max_gram", scalar(*max_gram)),
                    ],
                ),
                Tokenizer::Pattern { pattern } => {
                    tagged("pattern", vec![("pattern", Self::Text(pattern))])
                }
                #[cfg(feature = "nori")]
                Tokenizer::Nori(config) => morphology::nori(config)?,
                #[cfg(feature = "kuromoji")]
                Tokenizer::Kuromoji(config) => morphology::kuromoji(config)?,
            },
            Self::TokenFilter(config) => filter(config, snapshot)?,
            Self::CharFilter(config) => match config {
                CharFilter::HTMLStrip => tagged("html_strip", vec![]),
                CharFilter::CJKWidth => tagged("cjk_width", vec![]),
                CharFilter::Mapping { mapping } => {
                    tagged("mapping", vec![("mapping", Self::Mapping(mapping))])
                }
                CharFilter::PatternReplace {
                    pattern,
                    replacement,
                } => tagged(
                    "pattern_replace",
                    vec![
                        ("pattern", Self::Text(pattern)),
                        ("replacement", Self::Text(replacement)),
                    ],
                ),
                #[cfg(feature = "kuromoji")]
                CharFilter::KuromojiIterationMark {
                    normalize_kanji,
                    normalize_kana,
                } => tagged(
                    "kuromoji_iteration_mark",
                    vec![
                        ("normalize_kanji", scalar(*normalize_kanji)),
                        ("normalize_kana", scalar(*normalize_kana)),
                    ],
                ),
            },
            Self::Normalization(config) => match config {
                NormalizationConfig::Unavailable => tagged("unavailable", vec![]),
                NormalizationConfig::CJKWidth => tagged("cjk_width", vec![]),
                #[cfg(any(feature = "nori", feature = "kuromoji"))]
                NormalizationConfig::UnicodeSimpleLowercase { profile } => tagged(
                    "unicode_simple_lowercase",
                    vec![("profile", Self::Profile(profile))],
                ),
                #[cfg(any(feature = "nori", feature = "kuromoji"))]
                NormalizationConfig::CJKWidthSimpleLowercase { profile } => tagged(
                    "cjk_width_simple_lowercase",
                    vec![("profile", Self::Profile(profile))],
                ),
            },
            Self::Strings(values) => Form::Array(Array::Strings(values.iter())),
            Self::TokenFilters(values) => Form::Array(Array::TokenFilters(values.iter())),
            Self::CharFilters(values) => Form::Array(Array::CharFilters(values.iter())),
            Self::Mapping(values) => Form::Object(Object::Mapping(values.iter())),
            Self::Synonyms(values) => Form::Object(Object::Synonyms(values.iter())),
            Self::Text(value) => Form::Text(value),
            Self::Scalar(value) => Form::Scalar(value),
            #[cfg(feature = "nori")]
            Self::POSTags(values) => Form::Array(Array::POSTags(values.iter())),
            #[cfg(feature = "nori")]
            Self::POSTag(value) => Form::Scalar(serde_json::to_value(value)?),
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            Self::Profile(value) => morphology::profile(value),
        })
    }
}

#[cfg_attr(
    not(any(feature = "nori", feature = "kuromoji")),
    expect(
        clippy::unnecessary_wraps,
        reason = "language features add fallible public scalar serialization to this shared view"
    )
)]
fn filter(config: &TokenFilter, snapshot: bool) -> AnalysisResult<Form<'_>> {
    Ok(match config {
        TokenFilter::Lowercase => tagged("lowercase", vec![]),
        TokenFilter::PorterStem => tagged("porter_stem", vec![]),
        TokenFilter::ASCIIFolding => tagged("ascii_folding", vec![]),
        TokenFilter::Stop {
            language,
            custom_words,
        } => tagged(
            "stop",
            vec![
                ("language", View::Text(language)),
                ("custom_words", View::Strings(custom_words)),
            ],
        ),
        TokenFilter::Synonym {
            synonyms,
            synonyms_path,
        } => {
            let mut values = Vec::new();
            if snapshot || !synonyms.is_empty() {
                values.push(("synonyms", View::Synonyms(synonyms)));
            }
            if snapshot || synonyms_path.is_some() {
                // Descriptor input constructs paths from JSON strings, which are valid UTF-8.
                let path = synonyms_path
                    .as_ref()
                    .map(|path| path.to_str().expect("JSON configuration path"));
                values.push(("synonyms_path", optional_text(path)));
            }
            tagged("synonym", values)
        }
        TokenFilter::Ngram {
            min_gram,
            max_gram,
            keep_short,
        } => tagged(
            "ngram",
            vec![
                ("min_gram", scalar(*min_gram)),
                ("max_gram", scalar(*max_gram)),
                ("keep_short", scalar(*keep_short)),
            ],
        ),
        TokenFilter::EdgeNgram { min_gram, max_gram } => tagged(
            "edge_ngram",
            vec![
                ("min_gram", scalar(*min_gram)),
                ("max_gram", scalar(*max_gram)),
            ],
        ),
        TokenFilter::Length {
            min_length,
            max_length,
        } => tagged(
            "length",
            vec![
                ("min_length", scalar(*min_length)),
                ("max_length", scalar(*max_length)),
            ],
        ),
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        value => morphology::filter(value)?,
    })
}
