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
use super::morphology::{self, DictionaryWord, Morphology, WordEntry};
use super::{DictionaryId, DictionaryResult};
use crate::morphology::io::Reader;
use crate::morphology::lexicon::Lexicon;
use crate::morphology::unicode::{UnicodeProperties, UnicodeTable};

pub(super) mod provenance;
pub(super) mod tables;

use crate::morphology::matrix::Matrix;
use provenance::Provenance;
use tables::Characters;

pub use crate::morphology::limits::DictionaryLimits;

pub use crate::morphology::surfaces::SurfaceWords;

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
            let (lexicon, surfaces) =
                crate::morphology::surfaces::decode(reader, limits.max_text_utf16, known_words)?;
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
            Ok((unicode, crate::morphology::unicode::CODE_POINTS as usize))
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
        self.lexicon
            .prefixes(text)
            .map(|(length, rank)| (length, self.surfaces[rank as usize].clone()))
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
    crate::morphology::frame::take_section(sections, kind, decode)
}

fn read_section<T>(
    section: Section,
    decode: impl FnOnce(&mut Reader<'_>) -> DictionaryResult<(T, usize)>,
) -> DictionaryResult<T> {
    crate::morphology::frame::read_section(section, decode)
}
