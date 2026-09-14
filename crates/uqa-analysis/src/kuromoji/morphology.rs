//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese word records preserve all six independent nullable attributes.

use super::{DictionaryLimits, DictionaryResult};
use crate::morphology::io::{vector, Reader};

pub(super) const ABSENT: u32 = u32::MAX;

#[derive(Debug)]
pub(super) struct WordEntry {
    pub original_id: u32,
    pub left: u16,
    pub right: u16,
    pub cost: i16,
    pub attributes: [u32; 6],
}

#[derive(Debug)]
pub(super) struct Morphology {
    pub strings: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct DictionaryWord<'a> {
    pub(super) entry: &'a WordEntry,
    pub(super) morphology: &'a Morphology,
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
    pub fn part_of_speech(self) -> &'a str {
        &self.morphology.strings[self.entry.attributes[0] as usize]
    }
    pub fn base_form(self) -> Option<&'a str> {
        self.attribute(1)
    }
    pub fn reading(self) -> Option<&'a str> {
        self.attribute(2)
    }
    pub fn pronunciation(self) -> Option<&'a str> {
        self.attribute(3)
    }
    pub fn inflection_type(self) -> Option<&'a str> {
        self.attribute(4)
    }
    pub fn inflection_form(self) -> Option<&'a str> {
        self.attribute(5)
    }

    fn attribute(self, index: usize) -> Option<&'a str> {
        let id = self.entry.attributes[index];
        (id != ABSENT).then(|| self.morphology.strings[id as usize].as_str())
    }
}

impl Morphology {
    pub fn decode(reader: &mut Reader<'_>, limits: DictionaryLimits) -> DictionaryResult<Self> {
        Ok(Self {
            strings: crate::morphology::strings::decode(
                reader,
                limits.max_strings,
                limits.max_text_utf16,
            )?,
        })
    }

    #[cfg(any(test, feature = "kuromoji-tools"))]
    pub fn encode(&self, output: &mut crate::morphology::io::Writer) -> DictionaryResult<()> {
        crate::morphology::strings::encode(&self.strings, output).map_err(Into::into)
    }
}

pub(super) fn decode_words(
    reader: &mut Reader<'_>,
    morphology: &Morphology,
    forward: usize,
    backward: usize,
) -> DictionaryResult<(u32, Vec<WordEntry>)> {
    let known = reader.u32()?;
    let count = reader.count(36)?;
    if known as usize > count {
        return Err(reader.invalid("known word count exceeds word table").into());
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
        if reader.u16()? != 0 {
            return Err(reader.invalid("nonzero reserved word bytes").into());
        }
        let mut attributes = [ABSENT; 6];
        for id in &mut attributes {
            *id = reader.u32()?;
            if *id != ABSENT && *id as usize >= morphology.strings.len() {
                return Err(reader
                    .invalid("Japanese attribute string reference is out of range")
                    .into());
            }
        }
        if attributes[0] == ABSENT
            || original_id > i32::MAX as u32
            || left as usize >= backward
            || right as usize >= forward
        {
            return Err(reader
                .invalid("word identity, context or POS is invalid")
                .into());
        }
        words.push(WordEntry {
            original_id,
            left,
            right,
            cost,
            attributes,
        });
    }
    Ok((known, words))
}

#[cfg(any(test, feature = "kuromoji-tools"))]
pub(super) fn encode_words(
    words: &[WordEntry],
    known: u32,
    output: &mut crate::morphology::io::Writer,
) -> DictionaryResult<()> {
    output.u32(known)?;
    output.count(words.len())?;
    let mut previous_id = 0_i64;
    for word in words {
        let id = i64::from(word.original_id);
        let delta = i32::try_from(id - previous_id).map_err(|_| {
            super::error::invalid("word encoder", "original word ID delta exceeds i32")
        })?;
        output.i32(delta)?;
        previous_id = id;
        output.u16(word.left)?;
        output.u16(word.right)?;
        output.u16(word.cost as u16)?;
        output.u16(0)?;
        for id in word.attributes {
            output.u32(id)?;
        }
    }
    Ok(())
}
