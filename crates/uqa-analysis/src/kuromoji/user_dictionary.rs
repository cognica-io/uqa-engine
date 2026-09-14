//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable Japanese user phrases, segmentation, and independent morphology.

use std::sync::Arc;

use super::error::{check_limit, invalid};
use super::{DictionaryId, DictionaryResult, KuromojiDictionary};
use crate::morphology::io::vector;
use crate::morphology::lexicon::{Builder, Lexicon};

mod parse;

pub use crate::morphology::limits::UserDictionaryLimits;

const WORD_ID_OFFSET: u32 = 100_000_000;
const CONTEXT: u16 = 5;

#[derive(Debug)]
pub struct UserEntry {
    word_base: u32,
    lengths: Vec<usize>,
    readings: Vec<String>,
    pos: String,
}

impl UserEntry {
    pub fn word_base(&self) -> u32 {
        self.word_base
    }
    pub fn segment_lengths(&self) -> &[usize] {
        &self.lengths
    }
    pub fn left_context(&self) -> u16 {
        CONTEXT
    }
    pub fn right_context(&self) -> u16 {
        CONTEXT
    }
    pub fn cost(&self) -> i32 {
        -100_000
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UserWord<'a> {
    id: u32,
    reading: &'a str,
    pos: &'a str,
}

impl<'a> UserWord<'a> {
    pub fn id(self) -> u32 {
        self.id
    }
    pub fn left_context(self) -> u16 {
        CONTEXT
    }
    pub fn right_context(self) -> u16 {
        CONTEXT
    }
    pub fn cost(self) -> i32 {
        -100_000
    }
    pub fn reading(self) -> DictionaryResult<&'a str> {
        self.feature(0)
    }
    pub fn part_of_speech(self) -> DictionaryResult<&'a str> {
        self.feature(1)
    }
    pub fn base_form(self) -> Option<&'a str> {
        None
    }
    pub fn pronunciation(self) -> Option<&'a str> {
        None
    }
    pub fn inflection_type(self) -> Option<&'a str> {
        None
    }
    pub fn inflection_form(self) -> Option<&'a str> {
        None
    }

    // Lucene splits reading + NUL + POS and discards trailing empty fields.
    // Retain that observable access failure without duplicating POS per segment.
    fn feature(self, index: usize) -> DictionaryResult<&'a str> {
        let mut fields = self.reading.split('\0').chain(self.pos.split('\0'));
        let value = fields.nth(index);
        match value {
            Some(value) if !value.is_empty() || fields.any(|field| !field.is_empty()) => Ok(value),
            _ => Err(invalid(
                "user dictionary",
                "requested morphology field is absent",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserMatch {
    pub word_id: u32,
    pub start: usize,
    pub length: usize,
}

pub struct UserDictionary {
    model_id: DictionaryId,
    source: String,
    lexicon: Lexicon,
    entries: Vec<UserEntry>,
    word_entries: Vec<u32>,
}

impl std::fmt::Debug for UserDictionary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserDictionary")
            .field("model_id", &self.model_id)
            .field("entries", &self.entries.len())
            .field("words", &self.word_entries.len())
            .finish_non_exhaustive()
    }
}

impl UserDictionary {
    /// Compile the exact CSV source. Empty/comment-only input has no dictionary.
    pub fn compile(
        source: &str,
        model: &KuromojiDictionary,
        limits: UserDictionaryLimits,
    ) -> DictionaryResult<Option<Arc<Self>>> {
        check_limit("user dictionary bytes", source.len(), limits.max_bytes)?;
        let mut lines = parse::entries(source, limits.max_entries)?;
        if lines.is_empty() {
            return Ok(None);
        }
        let (forward, backward) = model.connection_shape();
        if forward <= usize::from(CONTEXT) || backward <= usize::from(CONTEXT) {
            return Err(invalid(
                "user dictionary",
                "model cannot address fixed user contexts",
            ));
        }
        lines.sort_by(|left, right| left[0].encode_utf16().cmp(right[0].encode_utf16()));
        let mut entries = vector(lines.len())?;
        let mut word_entries = Vec::new();
        let mut builder = Builder::new();
        for fields in &lines {
            let length = fields[0].encode_utf16().count();
            check_limit(
                "user surface UTF-16 units",
                length,
                limits.max_surface_utf16,
            )?;
            let mut surface = vector(length)?;
            surface.extend(fields[0].encode_utf16());
            let entry = prepare_entry(fields, word_entries.len())?;
            let phrase = u32::try_from(entries.len())
                .map_err(|_| invalid("user dictionary", "phrase ID exceeds u32"))?;
            word_entries.try_reserve(entry.lengths.len())?;
            word_entries.extend(std::iter::repeat_n(phrase, entry.lengths.len()));
            // Lucene rejects duplicate FST inputs, unlike Korean first-definition semantics.
            builder.insert(surface)?;
            entries.push(entry);
        }
        Ok(Some(Arc::new(Self {
            model_id: model.id(),
            source: parse::owned(source)?,
            lexicon: builder.finish(limits.max_surface_utf16)?,
            entries,
            word_entries,
        })))
    }

    pub fn model_id(&self) -> DictionaryId {
        self.model_id
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn entry(&self, phrase: u32) -> Option<&UserEntry> {
        self.entries.get(phrase as usize)
    }
    pub fn lookup(&self, text: &str) -> Option<u32> {
        self.lexicon.lookup(text.encode_utf16())
    }
    pub fn word(&self, id: u32) -> Option<UserWord<'_>> {
        let offset = id.checked_sub(WORD_ID_OFFSET)? as usize;
        let entry = self.entry(*self.word_entries.get(offset)?)?;
        Some(UserWord {
            id,
            reading: &entry.readings[(id - entry.word_base) as usize],
            pos: &entry.pos,
        })
    }
    pub(super) fn cursor(&self) -> crate::morphology::lexicon::Cursor<'_> {
        self.lexicon.cursor()
    }

    pub fn prefixes<'a>(&'a self, text: &'a [u16]) -> impl Iterator<Item = (usize, u32)> + 'a {
        self.lexicon.prefixes(text)
    }

    /// Longest phrase at every UTF-16 start, preserving overlapping matches and segment lengths.
    pub fn matches<'a>(&'a self, text: &'a [u16]) -> impl Iterator<Item = UserMatch> + 'a {
        (0..text.len()).flat_map(move |start| {
            let phrase = self
                .prefixes(&text[start..])
                .last()
                .map(|(_, phrase)| phrase);
            let entry = phrase.and_then(|phrase| self.entry(phrase));
            let mut position = start;
            entry.into_iter().flat_map(move |entry| {
                entry
                    .lengths
                    .iter()
                    .enumerate()
                    .map(move |(index, &length)| {
                        let result = UserMatch {
                            word_id: entry.word_base + index as u32,
                            start: position,
                            length,
                        };
                        position += length;
                        result
                    })
            })
        })
    }
}

fn prepare_entry(fields: &[String], words: usize) -> DictionaryResult<UserEntry> {
    let segments = parse::space_split(&fields[1])?;
    let readings = parse::space_split(&fields[2])?;
    if segments.len() != readings.len() {
        return Err(invalid(
            "user dictionary",
            "segmentation and reading counts differ",
        ));
    }
    if !parse::without_whitespace(&fields[0]).eq(parse::without_whitespace(&fields[1])) {
        return Err(invalid(
            "user dictionary",
            "concatenated segmentation differs from surface",
        ));
    }
    let word_base = u32::try_from(words)
        .ok()
        .and_then(|words| WORD_ID_OFFSET.checked_add(words))
        .filter(|base| {
            (i32::MAX as u32)
                .checked_sub(*base)
                .is_some_and(|remaining| segments.len() <= remaining as usize)
        })
        .ok_or_else(|| invalid("user dictionary", "word IDs exceed signed 32-bit range"))?;
    let mut lengths = vector(segments.len())?;
    let mut owned_readings = vector(readings.len())?;
    for (segment, reading) in segments.iter().zip(readings) {
        lengths.push(segment.encode_utf16().count());
        owned_readings.push(parse::owned(reading)?);
    }
    Ok(UserEntry {
        word_base,
        lengths,
        readings: owned_readings,
        pos: parse::owned(&fields[3])?,
    })
}

#[cfg(test)]
mod tests;
