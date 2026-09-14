//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable analyzer stop sets and ordered completion romanization mappings.

use super::{DictionaryLimits, DictionaryResult};
use crate::morphology::io::{vector, Reader};
use crate::morphology::lexicon::{Builder, Lexicon};

#[derive(Debug)]
pub struct CompletionMapping {
    pub(super) key: String,
    pub(super) alternatives: Vec<String>,
}

impl CompletionMapping {
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn alternatives(&self) -> &[String] {
        &self.alternatives
    }
}

#[derive(Debug)]
pub(super) struct AnalysisData {
    pub stop_words: Vec<String>,
    pub stop_tags: Vec<String>,
    pub completion: Vec<CompletionMapping>,
    pub completion_lexicon: Lexicon,
}

fn validate_order<'a>(values: impl IntoIterator<Item = &'a str>) -> DictionaryResult<()> {
    let mut previous: Option<&str> = None;
    for value in values {
        if value.is_empty()
            || previous
                .is_some_and(|previous| previous.encode_utf16().cmp(value.encode_utf16()).is_ge())
        {
            return Err(super::error::invalid(
                "analysis resources",
                "empty, duplicate or unordered key",
            ));
        }
        previous = Some(value);
    }
    Ok(())
}

impl AnalysisData {
    pub fn decode(reader: &mut Reader<'_>, limits: DictionaryLimits) -> DictionaryResult<Self> {
        let mut remaining = limits.max_strings;
        let stop_words =
            crate::morphology::strings::decode(reader, remaining, limits.max_text_utf16)?;
        remaining -= stop_words.len();
        let stop_tags =
            crate::morphology::strings::decode(reader, remaining, limits.max_text_utf16)?;
        remaining -= stop_tags.len();
        let count = reader.count(8)?;
        let minimum_strings = count
            .checked_mul(2)
            .ok_or_else(|| reader.invalid("string count overflow"))?;
        super::error::check_limit("analysis strings", minimum_strings, remaining)?;
        let mut completion = vector(count)?;
        for _ in 0..count {
            super::error::check_limit("analysis strings", 1, remaining)?;
            remaining -= 1;
            let text = reader.text()?;
            super::error::check_limit(
                "UTF-16 units per dictionary string",
                text.encode_utf16().count(),
                limits.max_text_utf16,
            )?;
            let mut key = String::new();
            key.try_reserve_exact(text.len())?;
            key.push_str(text);
            let alternatives =
                crate::morphology::strings::decode(reader, remaining, limits.max_text_utf16)?;
            remaining -= alternatives.len();
            completion.push(CompletionMapping { key, alternatives });
        }
        Self::new(stop_words, stop_tags, completion, limits.max_text_utf16)
    }

    pub fn new(
        stop_words: Vec<String>,
        stop_tags: Vec<String>,
        completion: Vec<CompletionMapping>,
        maximum_key_length: usize,
    ) -> DictionaryResult<Self> {
        validate_order(stop_words.iter().map(String::as_str))?;
        validate_order(stop_tags.iter().map(String::as_str))?;
        validate_order(completion.iter().map(|mapping| mapping.key.as_str()))?;
        let mut builder = Builder::new();
        for mapping in &completion {
            if mapping.alternatives.is_empty() || mapping.alternatives.iter().any(String::is_empty)
            {
                return Err(super::error::invalid(
                    "analysis resources",
                    "completion mapping has no nonempty alternatives",
                ));
            }
            let mut key = vector(mapping.key.encode_utf16().count())?;
            key.extend(mapping.key.encode_utf16());
            builder.insert(key)?;
        }
        let completion_lexicon = builder.finish(maximum_key_length)?;
        Ok(Self {
            stop_words,
            stop_tags,
            completion,
            completion_lexicon,
        })
    }

    #[cfg(any(test, feature = "kuromoji-tools"))]
    pub fn encode(&self, output: &mut crate::morphology::io::Writer) -> DictionaryResult<()> {
        crate::morphology::strings::encode(&self.stop_words, output)?;
        crate::morphology::strings::encode(&self.stop_tags, output)?;
        output.count(self.completion.len())?;
        for mapping in &self.completion {
            output.text(&mapping.key)?;
            crate::morphology::strings::encode(&mapping.alternatives, output)?;
        }
        Ok(())
    }
}
