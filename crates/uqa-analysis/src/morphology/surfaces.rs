//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical surface identities and contiguous word ranges for a UTF-16 lexicon.

use std::ops::Range;

use super::io::{vector, Reader};
use super::lexicon::Lexicon;
use super::DictionaryResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceWords {
    pub source_id: u32,
    pub word_ids: Range<u32>,
}

pub(crate) fn decode(
    reader: &mut Reader<'_>,
    maximum_text: usize,
    known_words: u32,
) -> DictionaryResult<(Lexicon, Vec<SurfaceWords>)> {
    let lexicon = Lexicon::decode(reader, maximum_text)?;
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
        let source_id =
            u32::try_from(id).map_err(|_| reader.invalid("source ID delta is out of range"))?;
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
        let word_ids = word_range(start, count, known_words as usize)?;
        next_word = word_ids.end;
        surfaces.push(SurfaceWords {
            source_id,
            word_ids,
        });
    }
    if next_word != known_words {
        return Err(reader.invalid("known words are not fully covered"));
    }
    Ok((lexicon, surfaces))
}

pub(crate) fn word_range(start: u32, count: u32, length: usize) -> DictionaryResult<Range<u32>> {
    let end = start
        .checked_add(count)
        .ok_or_else(|| super::error::invalid("word entries", "range overflow"))?;
    if end as usize > length {
        return Err(super::error::invalid("word entries", "range exceeds table"));
    }
    Ok(start..end)
}

#[cfg(any(test, feature = "nori-tools", feature = "kuromoji-tools"))]
pub(crate) fn encode(
    lexicon: &Lexicon,
    surfaces: &[SurfaceWords],
    output: &mut super::io::Writer,
) -> DictionaryResult<()> {
    lexicon.encode(output)?;
    output.count(surfaces.len())?;
    let mut previous_id = 0_i64;
    for surface in surfaces {
        let id = i64::from(surface.source_id);
        let delta = i32::try_from(id - previous_id)
            .map_err(|_| super::error::invalid("surface encoder", "source ID delta exceeds i32"))?;
        output.var_u32(((delta as u32) << 1) ^ ((delta >> 31) as u32))?;
        output.var_u32(surface.word_ids.end - surface.word_ids.start)?;
        previous_id = id;
    }
    Ok(())
}
