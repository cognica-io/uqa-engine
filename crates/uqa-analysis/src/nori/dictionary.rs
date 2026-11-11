//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable, fully validated dictionary ownership and lookup.

use std::ops::Range;
use std::sync::Arc;

use super::error::invalid;
use super::frame;
use super::morphology::{self, DictionaryWord, Morphology, WordEntry};
use super::{DictionaryId, DictionaryResult};
use crate::morphology::frame::take_section;
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
        crate::morphology::work::inline(Self::from_bytes_with(bytes, limits, &mut ()))
    }

    pub(crate) async fn from_bytes_with(
        bytes: &[u8],
        limits: DictionaryLimits,
        work: &mut impl crate::morphology::work::Work,
    ) -> DictionaryResult<Arc<Self>> {
        let (id, mut sections) = frame::decode_with(bytes, limits, work).await?;
        work.checkpoint().await?;
        let manifest = sections
            .pop()
            .ok_or_else(|| invalid("bundle", "missing provenance"))?;
        let provenance =
            Provenance::from_section(manifest, limits.max_manifest_bytes, work).await?;
        let matrix = {
            let section = take_section(&mut sections, 4)?;
            let mut reader = section.reader();
            let matrix = Matrix::decode_with(&mut reader, work).await?;
            section.finish(reader, matrix.costs.len())?;
            matrix
        };
        let morphology = {
            let section = take_section(&mut sections, 3)?;
            let mut reader = section.reader();
            let morphology = Morphology::decode_with(
                &mut reader,
                limits.max_strings,
                limits.max_text_utf16,
                work,
            )
            .await?;
            section.finish(reader, morphology.morphemes.len())?;
            morphology
        };
        let (known_words, words) = {
            let section = take_section(&mut sections, 2)?;
            let mut reader = section.reader();
            let words = morphology::decode_words_with(
                &mut reader,
                &morphology,
                matrix.forward,
                matrix.backward,
                work,
            )
            .await?;
            section.finish(reader, words.1.len())?;
            words
        };
        let (lexicon, surfaces) = {
            let section = take_section(&mut sections, 1)?;
            let mut reader = section.reader();
            let value = crate::morphology::surfaces::decode_with(
                &mut reader,
                limits.max_text_utf16,
                known_words,
                work,
            )
            .await?;
            section.finish(reader, value.1.len())?;
            value
        };
        let characters = {
            let section = take_section(&mut sections, 5)?;
            let mut reader = section.reader();
            let characters =
                Characters::decode_with(&mut reader, known_words, words.len(), work).await?;
            section.finish(reader, characters.values.len())?;
            characters
        };
        let unicode = {
            let section = take_section(&mut sections, 6)?;
            let mut reader = section.reader();
            let unicode =
                UnicodeTable::decode_with(&mut reader, provenance.scripts.len(), work).await?;
            section.finish(reader, crate::morphology::unicode::CODE_POINTS as usize)?;
            unicode
        };
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
        work.checkpoint().await?;
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
