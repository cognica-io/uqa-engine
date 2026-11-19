//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable analyzer stop sets and ordered completion romanization mappings.

use super::{DictionaryLimits, DictionaryResult};
use crate::morphology::io::{vector, Reader};
use crate::morphology::lexicon::{common_prefix_order, Builder, Lexicon};
use crate::morphology::work::Work;

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

async fn validate_order<'a>(
    values: impl IntoIterator<Item = &'a str>,
    work: &mut impl Work,
) -> DictionaryResult<()> {
    let mut previous: Option<&str> = None;
    for (index, value) in values.into_iter().enumerate() {
        work.records(index).await?;
        if value.is_empty() {
            return Err(super::error::invalid(
                "analysis resources",
                "empty, duplicate or unordered key",
            ));
        }
        if let Some(previous) = previous {
            let (_, order) =
                common_prefix_order(previous.encode_utf16(), value.encode_utf16(), work).await?;
            if !order.is_lt() {
                return Err(super::error::invalid(
                    "analysis resources",
                    "empty, duplicate or unordered key",
                ));
            }
        }
        previous = Some(value);
    }
    Ok(())
}

impl AnalysisData {
    pub async fn decode_with(
        reader: &mut Reader<'_>,
        limits: DictionaryLimits,
        work: &mut impl crate::morphology::work::Work,
    ) -> DictionaryResult<Self> {
        let mut remaining = limits.max_strings;
        let stop_words =
            crate::morphology::strings::decode_with(reader, remaining, limits.max_text_utf16, work)
                .await?;
        remaining -= stop_words.len();
        let stop_tags =
            crate::morphology::strings::decode_with(reader, remaining, limits.max_text_utf16, work)
                .await?;
        remaining -= stop_tags.len();
        let count = reader.count(8)?;
        let minimum_strings = count
            .checked_mul(2)
            .ok_or_else(|| reader.invalid("string count overflow"))?;
        super::error::check_limit("analysis strings", minimum_strings, remaining)?;
        let mut completion = vector(count)?;
        for index in 0..count {
            work.records(index).await?;
            super::error::check_limit("analysis strings", 1, remaining)?;
            remaining -= 1;
            let key =
                crate::morphology::strings::text_with(reader, limits.max_text_utf16, work).await?;
            let alternatives = crate::morphology::strings::decode_with(
                reader,
                remaining,
                limits.max_text_utf16,
                work,
            )
            .await?;
            remaining -= alternatives.len();
            completion.push(CompletionMapping { key, alternatives });
        }
        Self::new_with(
            stop_words,
            stop_tags,
            completion,
            limits.max_text_utf16,
            work,
        )
        .await
    }

    #[cfg(any(test, feature = "kuromoji-tools"))]
    pub fn new(
        stop_words: Vec<String>,
        stop_tags: Vec<String>,
        completion: Vec<CompletionMapping>,
        maximum_key_length: usize,
    ) -> DictionaryResult<Self> {
        crate::morphology::work::inline(Self::new_with(
            stop_words,
            stop_tags,
            completion,
            maximum_key_length,
            &mut (),
        ))
    }

    async fn new_with(
        stop_words: Vec<String>,
        stop_tags: Vec<String>,
        completion: Vec<CompletionMapping>,
        maximum_key_length: usize,
        work: &mut impl Work,
    ) -> DictionaryResult<Self> {
        validate_order(stop_words.iter().map(String::as_str), work).await?;
        validate_order(stop_tags.iter().map(String::as_str), work).await?;
        validate_order(completion.iter().map(CompletionMapping::key), work).await?;
        let mut builder = Builder::new();
        for mapping in &completion {
            if mapping.alternatives.is_empty() {
                return Err(super::error::invalid(
                    "analysis resources",
                    "completion mapping has no nonempty alternatives",
                ));
            }
            for (index, alternative) in mapping.alternatives.iter().enumerate() {
                work.records(index).await?;
                if alternative.is_empty() {
                    return Err(super::error::invalid(
                        "analysis resources",
                        "completion mapping has no nonempty alternatives",
                    ));
                }
            }
            let mut units = 0;
            for (index, character) in mapping.key.chars().enumerate() {
                work.records(index).await?;
                units += character.len_utf16();
            }
            let mut key = vector(units)?;
            for (index, unit) in mapping.key.encode_utf16().enumerate() {
                work.records(index).await?;
                key.push(unit);
            }
            builder = builder.insert_with(key, work).await?;
        }
        let completion_lexicon = builder.finish_with(maximum_key_length, work).await?;
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

#[cfg(test)]
mod tests;
