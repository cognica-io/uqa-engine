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
use crate::morphology::work::{ordering::Surface, source, Work};

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
        crate::morphology::work::inline(Self::compile_with(source, model, limits, &mut ()))
    }

    pub(crate) async fn compile_with(
        source: &str,
        model: &NoriDictionary,
        limits: UserDictionaryLimits,
        work: &mut impl Work,
    ) -> DictionaryResult<Option<Arc<Self>>> {
        check_limit("user dictionary bytes", source.len(), limits.max_bytes)?;
        let lines = parse::entries(source, limits.max_entries, work).await?;
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
        // Source indices break equal UTF-16 ties, retaining the first source definition.
        let order = crate::morphology::work::ordering::utf16(&lines, work).await?;
        let mut entries = crate::morphology::io::vector(lines.len())?;
        let mut builder = Builder::new();
        let mut previous = None;
        for (rank, index) in order.into_iter().enumerate() {
            work.records(rank).await?;
            let line = &lines[index];
            if let Some(previous) = previous {
                if crate::json::bytes_equal::<crate::morphology::error::DictionaryError>(
                    previous,
                    line.surface().as_bytes(),
                    work,
                )
                .await?
                {
                    continue;
                }
            }
            let length = source::utf16_len(line.surface(), work).await?;
            check_limit(
                "user surface UTF-16 units",
                length,
                limits.max_surface_utf16,
            )?;
            let surface = source::utf16(line.surface(), length, work).await?;
            entries.push(prepare_entry(line, surface.len(), model, forward, work).await?);
            builder = builder.insert_with(surface, work).await?;
            previous = Some(line.surface().as_bytes());
        }
        let lexicon = builder.finish_with(limits.max_surface_utf16, work).await?;
        let owned = source::text(source, work).await?;
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

async fn prepare_entry(
    line: &parse::Entry<'_>,
    surface_length: usize,
    model: &NoriDictionary,
    forward: usize,
    work: &mut impl Work,
) -> DictionaryResult<UserEntry> {
    let segments = if line.labels().len() == 0 {
        None
    } else {
        let mut lengths = crate::morphology::io::vector(line.labels().len())?;
        let mut total = 0_usize;
        for label in line.labels() {
            let length = source::utf16_len(label, work).await?;
            total = total
                .checked_add(length)
                .ok_or_else(|| invalid("user dictionary", "segmentation length overflow"))?;
            lengths.push(length);
        }
        if total > surface_length {
            return Err(invalid(
                "user dictionary",
                "segmentation is bigger than the surface form",
            ));
        }
        Some(lengths)
    };
    let mut units = [0; 2];
    let last = *line
        .original
        .chars()
        .next_back()
        .expect("nonempty rule")
        .encode_utf16(&mut units)
        .last()
        .expect("one or two UTF-16 units");
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
    Ok(UserEntry {
        right_context,
        segment_lengths: segments,
    })
}

#[cfg(test)]
mod tests;
