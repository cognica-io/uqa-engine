//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable, fully validated dictionary ownership and lookup.

use std::ops::Range;
use std::sync::Arc;

use super::error::invalid;
use super::frame::{self, Section};
use super::io::{vector, Reader};
use super::lexicon::Lexicon;
use super::morphology::{self, DictionaryWord, Morphology, WordEntry};
use super::unicode::{UnicodeProperties, UnicodeTable};
use super::{DictionaryId, DictionaryResult};

pub(super) mod provenance;
pub(super) mod tables;

use provenance::Provenance;
use tables::{Characters, Matrix};

/// Bounds for input bytes, decompressed section bytes, and decoded string metadata.
#[derive(Debug, Clone, Copy)]
pub struct DictionaryLimits {
    pub max_encoded_bytes: usize,
    pub max_decoded_bytes: usize,
    pub max_manifest_bytes: usize,
    pub max_text_utf16: usize,
    pub max_strings: usize,
}

impl Default for DictionaryLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 128 * 1024 * 1024,
            max_decoded_bytes: 256 * 1024 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_text_utf16: u16::MAX as usize,
            max_strings: 1_000_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceWords {
    pub source_id: u32,
    pub word_ids: Range<u32>,
}

pub struct NoriDictionary {
    id: DictionaryId,
    pub(super) known_words: u32,
    pub(super) lexicon: Lexicon,
    pub(super) surfaces: Vec<SurfaceWords>,
    pub(super) words: Vec<WordEntry>,
    pub(super) morphology: Morphology,
    pub(super) matrix: Matrix,
    pub(super) characters: Characters,
    pub(super) unicode: UnicodeTable,
    pub(super) provenance: Provenance,
}

impl std::fmt::Debug for NoriDictionary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NoriDictionary")
            .field("id", &self.id)
            .field("surfaces", &self.surfaces.len())
            .field("known_words", &self.known_words)
            .field("words", &self.words.len())
            .finish_non_exhaustive()
    }
}

impl NoriDictionary {
    /// Decode, validate, and publish one immutable model; no files or network resources are resolved.
    pub fn from_bytes(bytes: &[u8], limits: DictionaryLimits) -> DictionaryResult<Arc<Self>> {
        let (id, mut sections) = frame::decode(bytes, limits)?;
        let manifest = sections
            .pop()
            .ok_or_else(|| invalid("bundle", "missing provenance"))?;
        let provenance = read_section(manifest, |reader| {
            let bytes = reader.take(reader.remaining())?;
            Ok((Provenance::decode(bytes, limits.max_manifest_bytes)?, 1))
        })?;
        let matrix = take_section(&mut sections, 4, |reader| {
            let matrix = Matrix::decode(reader)?;
            let count = matrix.costs.len();
            Ok((matrix, count))
        })?;
        let morphology = take_section(&mut sections, 3, |reader| {
            let morphology = Morphology::decode(reader, limits.max_strings, limits.max_text_utf16)?;
            let count = morphology.morphemes.len();
            Ok((morphology, count))
        })?;
        let (known_words, words) = take_section(&mut sections, 2, |reader| {
            let words =
                morphology::decode_words(reader, &morphology, matrix.forward, matrix.backward)?;
            let count = words.1.len();
            Ok((words, count))
        })?;
        let (lexicon, surfaces) = take_section(&mut sections, 1, |reader| {
            let lexicon = Lexicon::decode(reader, limits.max_text_utf16)?;
            let count = reader.count(2)?;
            if count != lexicon.len() {
                return Err(reader.invalid("surface table and lexicon lengths differ"));
            }
            let mut seen = vector(count)?;
            seen.resize(count, false);
            let mut surfaces = vector(count)?;
            let mut next_word = 0;
            let mut previous_id = 0_i64;
            for _ in 0..count {
                let delta = reader.var_u32()?;
                let delta = ((delta >> 1) as i32) ^ -((delta & 1) as i32);
                let id = previous_id + i64::from(delta);
                let source_id = u32::try_from(id)
                    .map_err(|_| reader.invalid("source ID delta is out of range"))?;
                previous_id = id;
                let start = next_word;
                let count = reader.var_u32()?;
                if source_id as usize >= seen.len()
                    || seen[source_id as usize]
                    || start != next_word
                    || count == 0
                {
                    return Err(reader.invalid("invalid source identity or word range"));
                }
                seen[source_id as usize] = true;
                let word_ids = morphology::word_range(start, count, known_words as usize)?;
                next_word = word_ids.end;
                surfaces.push(SurfaceWords {
                    source_id,
                    word_ids,
                });
            }
            if next_word != known_words {
                return Err(reader.invalid("known words are not fully covered"));
            }
            let count = surfaces.len();
            Ok(((lexicon, surfaces), count))
        })?;
        let characters = take_section(&mut sections, 5, |reader| {
            let characters = Characters::decode(reader, known_words, words.len())?;
            let count = characters.values.len();
            Ok((characters, count))
        })?;
        let unicode = take_section(&mut sections, 6, |reader| {
            let unicode = UnicodeTable::decode(reader, provenance.scripts.len())?;
            Ok((unicode, super::unicode::CODE_POINTS as usize))
        })?;
        let dictionary = Self {
            id,
            known_words,
            lexicon,
            surfaces,
            words,
            morphology,
            matrix,
            characters,
            unicode,
            provenance,
        };
        dictionary.provenance.validate_counts(&dictionary)?;
        Ok(Arc::new(dictionary))
    }

    pub fn id(&self) -> DictionaryId {
        self.id
    }
    pub fn surface_count(&self) -> usize {
        self.surfaces.len()
    }
    pub fn known_word_count(&self) -> usize {
        self.known_words as usize
    }
    pub fn word_count(&self) -> usize {
        self.words.len()
    }
    pub fn connection_shape(&self) -> (usize, usize) {
        (self.matrix.forward, self.matrix.backward)
    }
    pub fn provenance(&self) -> &serde_json::Value {
        &self.provenance.json
    }

    /// Exact surface lookup; dense word IDs are scoped to this dictionary identity.
    pub fn lookup(&self, text: &str) -> Option<SurfaceWords> {
        let rank = self.lexicon.lookup(text.encode_utf16())?;
        Some(self.surfaces[rank as usize].clone())
    }

    /// Nonempty accepted prefixes, in increasing UTF-16 unit length.
    pub fn prefixes<'a>(
        &'a self,
        text: &'a [u16],
    ) -> impl Iterator<Item = (usize, SurfaceWords)> + 'a {
        let mut cursor = self.lexicon.cursor();
        let mut index = 0;
        let mut ended = false;
        std::iter::from_fn(move || {
            if ended {
                return None;
            }
            while let Some(&label) = text.get(index) {
                index += 1;
                if cursor.advance(label).is_none() {
                    ended = true;
                    return None;
                }
                if let Some(rank) = cursor.rank() {
                    return Some((index, self.surfaces[rank as usize].clone()));
                }
            }
            ended = true;
            None
        })
    }

    pub fn word(&self, id: u32) -> Option<DictionaryWord<'_>> {
        self.words.get(id as usize).map(|entry| DictionaryWord {
            entry,
            morphology: &self.morphology,
        })
    }

    pub fn unknown_words(&self, class: u8) -> Option<Range<u32>> {
        self.characters.words.get(class as usize).cloned()
    }

    pub fn connection_cost(&self, forward: u16, backward: u16) -> Option<i16> {
        self.matrix.get(forward as usize, backward as usize)
    }

    pub fn character_class(&self, unit: u16) -> u8 {
        self.characters.values[unit as usize][0]
    }
    pub fn character_morphology_flags(&self, unit: u16) -> u8 {
        self.characters.values[unit as usize][1]
    }
    pub fn invokes_unknown(&self, unit: u16) -> bool {
        self.characters.flags[self.character_class(unit) as usize] & 1 != 0
    }
    pub fn groups_unknown(&self, unit: u16) -> bool {
        self.characters.flags[self.character_class(unit) as usize] & 2 != 0
    }
    pub fn unicode(&self, code_point: u32) -> Option<UnicodeProperties> {
        self.unicode.get(code_point)
    }
    pub fn unicode_script_name(&self, script: u16) -> Option<&str> {
        self.provenance
            .scripts
            .get(script as usize)
            .map(String::as_str)
    }
}

fn take_section<T>(
    sections: &mut Vec<Section>,
    kind: u32,
    decode: impl FnOnce(&mut Reader<'_>) -> DictionaryResult<(T, usize)>,
) -> DictionaryResult<T> {
    let index = sections
        .iter()
        .position(|section| section.kind == kind)
        .ok_or_else(|| invalid("bundle", "missing section"))?;
    read_section(sections.remove(index), decode)
}

fn read_section<T>(
    section: Section,
    decode: impl FnOnce(&mut Reader<'_>) -> DictionaryResult<(T, usize)>,
) -> DictionaryResult<T> {
    let mut reader = Reader::new(&section.bytes, "dictionary section", false);
    let (value, count) = decode(&mut reader)?;
    if count as u64 != section.records {
        return Err(reader.invalid("directory and decoded record counts differ"));
    }
    reader.finish()?;
    drop(section);
    Ok(value)
}
