//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow native configuration fields without materializing another complete pipeline.

use std::collections::{btree_map, BTreeMap};

mod scalar;
pub(super) use scalar::Scalar;

use crate::{Analyzer, CharFilter, NormalizationConfig, TokenFilter, Tokenizer};

mod morphology;

#[derive(Clone, Copy)]
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
    Path(&'a std::path::Path),
    Scalar(Scalar),
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
    Path(&'a std::path::Path),
    Scalar(Scalar),
}

pub(super) enum Object<'a> {
    Fields {
        view: View<'a>,
        snapshot: bool,
        index: usize,
        remaining: usize,
    },
    Mapping(btree_map::Iter<'a, String, String>),
    Synonyms(btree_map::Iter<'a, String, Vec<String>>),
}

impl<'a> Iterator for Object<'a> {
    type Item = (&'a str, View<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Fields {
                view,
                snapshot,
                index,
                remaining,
            } => {
                if *remaining == 0 {
                    return None;
                }
                let mut fields = view.fields(*snapshot);
                let value = fields.values[*index].take();
                *index += 1;
                *remaining -= 1;
                value
            }
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
            Self::Fields { remaining, .. } => *remaining,
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

fn scalar<'a>(value: impl Into<Scalar>) -> View<'a> {
    View::Scalar(value.into())
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
fn optional_text(value: Option<&str>) -> View<'_> {
    value.map_or(View::Scalar(Scalar::Null), View::Text)
}

struct Fields<'a> {
    values: [Option<(&'a str, View<'a>)>; 8],
    length: usize,
}

impl<'a> Fields<'a> {
    fn push(&mut self, value: (&'a str, View<'a>)) {
        self.values[self.length] = Some(value);
        self.length += 1;
    }
}

fn fields<'a, const N: usize>(values: [(&'a str, View<'a>); N]) -> Fields<'a> {
    let mut fields = Fields {
        values: [None; 8],
        length: 0,
    };
    for value in values {
        fields.push(value);
    }
    fields
}

fn tagged<'a, const N: usize>(tag: &'static str, values: [(&'a str, View<'a>); N]) -> Fields<'a> {
    let mut fields = fields([("type", View::Text(tag))]);
    for value in values {
        fields.push(value);
    }
    fields
}

impl<'a> View<'a> {
    pub(super) fn form(self, snapshot: bool) -> Form<'a> {
        match self {
            Self::Strings(values) => Form::Array(Array::Strings(values.iter())),
            Self::TokenFilters(values) => Form::Array(Array::TokenFilters(values.iter())),
            Self::CharFilters(values) => Form::Array(Array::CharFilters(values.iter())),
            Self::Mapping(values) => Form::Object(Object::Mapping(values.iter())),
            Self::Synonyms(values) => Form::Object(Object::Synonyms(values.iter())),
            Self::Text(value) => Form::Text(value),
            Self::Path(value) => Form::Path(value),
            Self::Scalar(value) => Form::Scalar(value),
            #[cfg(feature = "nori")]
            Self::POSTags(values) => Form::Array(Array::POSTags(values.iter())),
            #[cfg(feature = "nori")]
            Self::POSTag(value) => Form::Scalar(Scalar::POSTag(*value)),
            value => Form::Object(Object::Fields {
                view: value,
                snapshot,
                index: 0,
                remaining: value.fields(snapshot).length,
            }),
        }
    }

    /// Reify at most eight fixed fields on the stack; containers retain their original iterators.
    fn fields(self, snapshot: bool) -> Fields<'a> {
        match self {
            Self::Analyzer(config) => {
                let mut values = fields([
                    ("tokenizer", Self::Tokenizer(&config.tokenizer)),
                    ("token_filters", Self::TokenFilters(&config.token_filters)),
                    ("char_filters", Self::CharFilters(&config.char_filters)),
                ]);
                if let Some(value) = &config.normalization {
                    values.push(("normalization", Self::Normalization(value)));
                }
                values
            }
            Self::Tokenizer(config) => match config {
                Tokenizer::Whitespace => tagged("whitespace", []),
                Tokenizer::Standard => tagged("standard", []),
                Tokenizer::Letter => tagged("letter", []),
                Tokenizer::Keyword => tagged("keyword", []),
                Tokenizer::NGram { min_gram, max_gram } => tagged(
                    "n_gram",
                    [
                        ("min_gram", scalar(*min_gram)),
                        ("max_gram", scalar(*max_gram)),
                    ],
                ),
                Tokenizer::Pattern { pattern } => {
                    tagged("pattern", [("pattern", Self::Text(pattern))])
                }
                #[cfg(feature = "nori")]
                Tokenizer::Nori(config) => morphology::nori(config),
                #[cfg(feature = "kuromoji")]
                Tokenizer::Kuromoji(config) => morphology::kuromoji(config),
            },
            Self::TokenFilter(config) => filter(config, snapshot),
            Self::CharFilter(config) => match config {
                CharFilter::HTMLStrip => tagged("html_strip", []),
                CharFilter::CJKWidth => tagged("cjk_width", []),
                CharFilter::Mapping { mapping } => {
                    tagged("mapping", [("mapping", Self::Mapping(mapping))])
                }
                CharFilter::PatternReplace {
                    pattern,
                    replacement,
                } => tagged(
                    "pattern_replace",
                    [
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
                    [
                        ("normalize_kanji", scalar(*normalize_kanji)),
                        ("normalize_kana", scalar(*normalize_kana)),
                    ],
                ),
            },
            Self::Normalization(config) => match config {
                NormalizationConfig::Unavailable => tagged("unavailable", []),
                NormalizationConfig::CJKWidth => tagged("cjk_width", []),
                #[cfg(any(feature = "nori", feature = "kuromoji"))]
                NormalizationConfig::UnicodeSimpleLowercase { profile } => tagged(
                    "unicode_simple_lowercase",
                    [("profile", Self::Profile(profile))],
                ),
                #[cfg(any(feature = "nori", feature = "kuromoji"))]
                NormalizationConfig::CJKWidthSimpleLowercase { profile } => tagged(
                    "cjk_width_simple_lowercase",
                    [("profile", Self::Profile(profile))],
                ),
            },
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            Self::Profile(value) => morphology::profile(value),
            _ => unreachable!("only native objects project fixed fields"),
        }
    }
}

fn filter(config: &TokenFilter, snapshot: bool) -> Fields<'_> {
    match config {
        TokenFilter::Lowercase => tagged("lowercase", []),
        TokenFilter::PorterStem => tagged("porter_stem", []),
        TokenFilter::ASCIIFolding => tagged("ascii_folding", []),
        TokenFilter::Stop {
            language,
            custom_words,
        } => tagged(
            "stop",
            [
                ("language", View::Text(language)),
                ("custom_words", View::Strings(custom_words)),
            ],
        ),
        TokenFilter::Synonym {
            synonyms,
            synonyms_path,
        } => {
            let mut values = tagged("synonym", []);
            if snapshot || !synonyms.is_empty() {
                values.push(("synonyms", View::Synonyms(synonyms)));
            }
            if snapshot || synonyms_path.is_some() {
                values.push((
                    "synonyms_path",
                    synonyms_path
                        .as_deref()
                        .map_or(View::Scalar(Scalar::Null), View::Path),
                ));
            }
            values
        }
        TokenFilter::Ngram {
            min_gram,
            max_gram,
            keep_short,
        } => tagged(
            "ngram",
            [
                ("min_gram", scalar(*min_gram)),
                ("max_gram", scalar(*max_gram)),
                ("keep_short", scalar(*keep_short)),
            ],
        ),
        TokenFilter::EdgeNgram { min_gram, max_gram } => tagged(
            "edge_ngram",
            [
                ("min_gram", scalar(*min_gram)),
                ("max_gram", scalar(*max_gram)),
            ],
        ),
        TokenFilter::Length {
            min_length,
            max_length,
        } => tagged(
            "length",
            [
                ("min_length", scalar(*min_length)),
                ("max_length", scalar(*max_length)),
            ],
        ),
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        value => morphology::filter(value),
    }
}
