//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compact word entries referencing shared reading and morpheme tables.

use std::ops::Range;

use super::error::{check_limit, invalid};
use super::io::{vector, Reader};
use super::DictionaryResult;

mod pos;
pub use pos::{POSTag, POSType};

pub(super) const ABSENT: u32 = u32::MAX;

#[derive(Debug)]
pub(super) struct WordEntry {
    pub original_id: u32,
    pub left: u16,
    pub right: u16,
    pub cost: i16,
    pub pos_type: POSType,
    pub left_pos: POSTag,
    pub right_pos: POSTag,
    pub reading: u32,
    pub morphemes: u32,
    pub morpheme_count: u32,
}

#[derive(Debug)]
pub(super) struct Morpheme {
    pub surface: u32,
    pub pos: POSTag,
}

#[derive(Debug)]
pub(super) struct Morphology {
    pub strings: Vec<String>,
    pub morphemes: Vec<Morpheme>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MorphemeRef<'a> {
    pub surface: &'a str,
    pub pos: POSTag,
}

#[derive(Clone, Copy)]
pub struct DictionaryWord<'a> {
    pub(super) entry: &'a WordEntry,
    pub(super) morphology: &'a Morphology,
}

impl std::fmt::Debug for DictionaryWord<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DictionaryWord")
            .field("entry", self.entry)
            .field("reading", &self.reading())
            .finish_non_exhaustive()
    }
}

impl<'a> DictionaryWord<'a> {
    pub fn original_id(self) -> u32 {
        self.entry.original_id
    }
    pub fn left_context(self) -> u16 {
        self.entry.left
    }
    pub fn right_context(self) -> u16 {
        self.entry.right
    }
    pub fn cost(self) -> i16 {
        self.entry.cost
    }
    pub fn pos_type(self) -> POSType {
        self.entry.pos_type
    }
    pub fn left_pos(self) -> POSTag {
        self.entry.left_pos
    }
    pub fn right_pos(self) -> POSTag {
        self.entry.right_pos
    }

    pub fn reading(self) -> Option<&'a str> {
        (self.entry.reading != ABSENT)
            .then(|| self.morphology.strings[self.entry.reading as usize].as_str())
    }

    pub fn morphemes(self) -> Option<impl ExactSizeIterator<Item = MorphemeRef<'a>> + 'a> {
        if self.entry.morphemes == ABSENT {
            return None;
        }
        let start = self.entry.morphemes as usize;
        let end = start + self.entry.morpheme_count as usize;
        Some(
            self.morphology.morphemes[start..end]
                .iter()
                .map(move |item| MorphemeRef {
                    surface: &self.morphology.strings[item.surface as usize],
                    pos: item.pos,
                }),
        )
    }
}

impl Morphology {
    pub fn decode(
        reader: &mut Reader<'_>,
        maximum_strings: usize,
        maximum_text: usize,
    ) -> DictionaryResult<Self> {
        let count = reader.count(4)?;
        check_limit("dictionary strings", count, maximum_strings)?;
        let mut strings = vector(count)?;
        for _ in 0..count {
            let text = reader.text()?;
            check_limit(
                "UTF-16 units per dictionary string",
                text.encode_utf16().count(),
                maximum_text,
            )?;
            let mut owned = String::new();
            owned.try_reserve_exact(text.len())?;
            owned.push_str(text);
            strings.push(owned);
        }
        let count = reader.count(5)?;
        let mut morphemes = vector(count)?;
        for _ in 0..count {
            let surface = reader.u32()?;
            let pos = POSTag::from_ordinal(reader.u8()?)?;
            if surface as usize >= strings.len() {
                return Err(reader.invalid("morpheme string reference is out of range"));
            }
            morphemes.push(Morpheme { surface, pos });
        }
        Ok(Self { strings, morphemes })
    }

    #[cfg(any(test, feature = "nori-tools"))]
    pub fn encode(&self, output: &mut super::io::Writer) -> DictionaryResult<()> {
        output.count(self.strings.len())?;
        for string in &self.strings {
            output.text(string)?;
        }
        output.count(self.morphemes.len())?;
        for morpheme in &self.morphemes {
            output.u32(morpheme.surface)?;
            output.u8(morpheme.pos.ordinal())?;
        }
        Ok(())
    }
}

pub(super) fn decode_words(
    reader: &mut Reader<'_>,
    morphology: &Morphology,
    forward: usize,
    backward: usize,
) -> DictionaryResult<(u32, Vec<WordEntry>)> {
    let known = reader.u32()?;
    let count = reader.count(32)?;
    if known as usize > count {
        return Err(reader.invalid("known word count exceeds word table"));
    }
    let mut words = vector(count)?;
    let mut previous_id = 0_i64;
    for _ in 0..count {
        let id = previous_id + i64::from(reader.i32()?);
        let original_id = u32::try_from(id)
            .map_err(|_| reader.invalid("original word ID delta is out of range"))?;
        previous_id = id;
        let left = reader.u16()?;
        let right = reader.u16()?;
        let cost = reader.u16()? as i16;
        let pos_type = POSType::from_ordinal(reader.u8()?)?;
        let left_pos = POSTag::from_ordinal(reader.u8()?)?;
        let right_pos = POSTag::from_ordinal(reader.u8()?)?;
        if reader.take(3)? != [0, 0, 0] {
            return Err(reader.invalid("nonzero reserved word bytes"));
        }
        let reading = reader.u32()?;
        let morphemes = reader.u32()?;
        let morpheme_count = reader.u32()?;
        if reader.u32()? != 0 {
            return Err(reader.invalid("nonzero reserved word bytes"));
        }
        if original_id > i32::MAX as u32 || left as usize >= backward || right as usize >= forward {
            return Err(reader.invalid("word ID or context is out of range"));
        }
        if reading != ABSENT && reading as usize >= morphology.strings.len() {
            return Err(reader.invalid("reading string reference is out of range"));
        }
        if morphemes == ABSENT {
            if morpheme_count != 0 {
                return Err(reader.invalid("absent morphemes have a nonzero count"));
            }
        } else {
            word_range(morphemes, morpheme_count, morphology.morphemes.len())?;
        }
        words.push(WordEntry {
            original_id,
            left,
            right,
            cost,
            pos_type,
            left_pos,
            right_pos,
            reading,
            morphemes,
            morpheme_count,
        });
    }
    Ok((known, words))
}

pub(super) fn word_range(start: u32, count: u32, length: usize) -> DictionaryResult<Range<u32>> {
    let end = start
        .checked_add(count)
        .ok_or_else(|| invalid("word entries", "range overflow"))?;
    if end as usize > length {
        return Err(invalid("word entries", "range exceeds table"));
    }
    Ok(start..end)
}

#[cfg(any(test, feature = "nori-tools"))]
pub(super) fn encode_words(
    words: &[WordEntry],
    known: u32,
    output: &mut super::io::Writer,
) -> DictionaryResult<()> {
    output.u32(known)?;
    output.count(words.len())?;
    let mut previous_id = 0_i64;
    for word in words {
        let id = i64::from(word.original_id);
        let delta = i32::try_from(id - previous_id)
            .map_err(|_| invalid("word encoder", "original word ID delta exceeds i32"))?;
        output.i32(delta)?;
        previous_id = id;
        output.u16(word.left)?;
        output.u16(word.right)?;
        output.u16(word.cost as u16)?;
        output.u8(word.pos_type as u8)?;
        output.u8(word.left_pos.ordinal())?;
        output.u8(word.right_pos.ordinal())?;
        output.bytes(&[0, 0, 0])?;
        output.u32(word.reading)?;
        output.u32(word.morphemes)?;
        output.u32(word.morpheme_count)?;
        output.u32(0)?;
    }
    Ok(())
}
