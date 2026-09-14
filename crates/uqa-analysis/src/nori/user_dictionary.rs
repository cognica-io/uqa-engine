//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable user nouns with Lucene's ordered UTF-16 segmentation semantics.

use std::sync::Arc;

use super::error::{check_limit, invalid};
use super::{DictionaryId, DictionaryResult, NoriDictionary, POSTag, POSType};
use crate::morphology::lexicon::{Builder, Lexicon};

mod parse;

pub(super) const LEFT_CONTEXT: u16 = 1781;
pub(super) const WORD_COST: i32 = -100_000;

pub use crate::morphology::limits::UserDictionaryLimits;

#[derive(Debug)]
pub struct UserEntry {
    right_context: u16,
    segment_lengths: Option<Vec<usize>>,
}

impl UserEntry {
    pub fn left_context(&self) -> u16 {
        LEFT_CONTEXT
    }
    pub fn right_context(&self) -> u16 {
        self.right_context
    }
    pub fn cost(&self) -> i32 {
        WORD_COST
    }
    pub fn pos(&self) -> POSTag {
        POSTag::NNG
    }
    pub fn pos_type(&self) -> POSType {
        if self.segment_lengths.is_some() {
            POSType::Compound
        } else {
            POSType::Morpheme
        }
    }
    /// Lengths select consecutive UTF-16 slices from the matched surface, regardless of label text.
    pub fn segment_lengths(&self) -> Option<&[usize]> {
        self.segment_lengths.as_deref()
    }
}

pub struct UserDictionary {
    model_id: DictionaryId,
    source: String,
    lexicon: Lexicon,
    entries: Vec<UserEntry>,
}

impl std::fmt::Debug for UserDictionary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserDictionary")
            .field("model_id", &self.model_id)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl UserDictionary {
    /// Compile exactly the supplied rules against one model. An empty/comment-only source has no dictionary.
    pub fn compile(
        source: &str,
        model: &NoriDictionary,
        limits: UserDictionaryLimits,
    ) -> DictionaryResult<Option<Arc<Self>>> {
        check_limit("user dictionary bytes", source.len(), limits.max_bytes)?;
        let mut lines = parse::entries(source, limits.max_entries)?;
        if lines.is_empty() {
            return Ok(None);
        }
        let (forward, backward) = model.connection_shape();
        if backward <= usize::from(LEFT_CONTEXT) {
            return Err(invalid(
                "user dictionary",
                "model cannot address fixed user-noun contexts",
            ));
        }
        // Stable sorting retains the first source definition among equal UTF-16 surfaces.
        lines.sort_by(|left, right| {
            left.surface()
                .encode_utf16()
                .cmp(right.surface().encode_utf16())
        });
        let mut entries = crate::morphology::io::vector(lines.len())?;
        let mut builder = Builder::new();
        let mut previous = None;
        for line in &lines {
            if previous == Some(line.surface()) {
                continue;
            }
            let surface: Vec<_> = line.surface().encode_utf16().collect();
            check_limit(
                "user surface UTF-16 units",
                surface.len(),
                limits.max_surface_utf16,
            )?;
            let segments = if line.labels().len() == 0 {
                None
            } else {
                let mut lengths = crate::morphology::io::vector(line.labels().len())?;
                let mut total = 0_usize;
                for label in line.labels() {
                    let length = label.encode_utf16().count();
                    total = total.checked_add(length).ok_or_else(|| {
                        invalid("user dictionary", "segmentation length overflow")
                    })?;
                    lengths.push(length);
                }
                if total > surface.len() {
                    return Err(invalid(
                        "user dictionary",
                        "segmentation is bigger than the surface form",
                    ));
                }
                Some(lengths)
            };
            let last = line.original.encode_utf16().last().expect("nonempty rule");
            let flags = model.character_morphology_flags(last);
            let right_context = if flags & 2 == 0 {
                3533
            } else if flags & 4 == 0 {
                3534
            } else {
                3535
            };
            if forward <= usize::from(right_context) {
                return Err(invalid(
                    "user dictionary",
                    "model cannot address the user-noun right context",
                ));
            }
            entries.push(UserEntry {
                right_context,
                segment_lengths: segments,
            });
            builder.insert(surface)?;
            previous = Some(line.surface());
        }
        let lexicon = builder.finish(limits.max_surface_utf16)?;
        let mut owned = String::new();
        owned.try_reserve_exact(source.len())?;
        owned.push_str(source);
        Ok(Some(Arc::new(Self {
            model_id: model.id(),
            source: owned,
            lexicon,
            entries,
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
    pub fn entry(&self, id: u32) -> Option<&UserEntry> {
        self.entries.get(id as usize)
    }
    pub fn lookup(&self, text: &str) -> Option<u32> {
        self.lexicon.lookup(text.encode_utf16())
    }

    pub(super) fn cursor(&self) -> crate::morphology::lexicon::Cursor<'_> {
        self.lexicon.cursor()
    }

    pub fn prefixes<'a>(&'a self, text: &'a [u16]) -> impl Iterator<Item = (usize, u32)> + 'a {
        self.lexicon.prefixes(text)
    }
}
