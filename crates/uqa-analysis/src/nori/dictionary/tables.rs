//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dense connection costs and the complete UTF-16 unknown-character model.

use std::ops::Range;

use crate::nori::io::{vector, Reader};
use crate::nori::morphology::word_range;
use crate::nori::DictionaryResult;

pub(in crate::nori) const CLASSES: &[&str] = &[
    "NGRAM",
    "DEFAULT",
    "SPACE",
    "SYMBOL",
    "NUMERIC",
    "ALPHA",
    "CYRILLIC",
    "GREEK",
    "HIRAGANA",
    "KATAKANA",
    "KANJI",
    "HANGUL",
    "HANJA",
    "HANJANUMERIC",
];

#[derive(Debug)]
pub(in crate::nori) struct Matrix {
    pub forward: usize,
    pub backward: usize,
    pub costs: Vec<i16>,
}

impl Matrix {
    pub fn get(&self, forward: usize, backward: usize) -> Option<i16> {
        if forward >= self.forward || backward >= self.backward {
            return None;
        }
        Some(self.costs[backward * self.forward + forward])
    }

    pub fn decode(reader: &mut Reader<'_>) -> DictionaryResult<Self> {
        let forward = reader.u32()? as usize;
        let backward = reader.u32()? as usize;
        let count = forward
            .checked_mul(backward)
            .ok_or_else(|| reader.invalid("matrix size overflow"))?;
        if forward == 0
            || backward == 0
            || forward > 0x1_0000
            || backward > 0x1_0000
            || count > reader.remaining() / 2
        {
            return Err(reader.invalid("invalid connection matrix dimensions"));
        }
        let mut costs = vector(count)?;
        for _ in 0..count {
            costs.push(reader.u16()? as i16);
        }
        Ok(Self {
            forward,
            backward,
            costs,
        })
    }

    #[cfg(any(test, feature = "nori-tools"))]
    pub fn encode(&self, output: &mut crate::nori::io::Writer) -> DictionaryResult<()> {
        output.count(self.forward)?;
        output.count(self.backward)?;
        for cost in &self.costs {
            output.u16(*cost as u16)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(in crate::nori) struct Characters {
    pub flags: Vec<u8>,
    pub words: Vec<Range<u32>>,
    pub values: Vec<[u8; 2]>,
}

impl Characters {
    pub fn decode(reader: &mut Reader<'_>, known: u32, total: usize) -> DictionaryResult<Self> {
        if reader.u32()? as usize != CLASSES.len() {
            return Err(reader.invalid("unknown character class vocabulary"));
        }
        let mut flags = vector(CLASSES.len())?;
        flags.extend_from_slice(reader.take(CLASSES.len())?);
        if flags.iter().any(|flag| *flag > 3) {
            return Err(reader.invalid("invalid unknown-character flags"));
        }
        let mut words = vector(CLASSES.len())?;
        let mut next = known;
        for _ in CLASSES {
            let start = reader.u32()?;
            let count = reader.u32()?;
            if start != next || count == 0 {
                return Err(reader.invalid("unknown word ranges are empty or noncontiguous"));
            }
            let range = word_range(start, count, total)?;
            next = range.end;
            words.push(range);
        }
        if next as usize != total || reader.u32()? != 0x1_0000 {
            return Err(reader.invalid("incomplete unknown words or UTF-16 character table"));
        }
        if reader.remaining() < 0x2_0000 {
            return Err(reader.invalid("truncated UTF-16 character table"));
        }
        let mut values = vector(0x1_0000)?;
        for unit in 0..0x1_0000_i32 {
            let class = reader.u8()?;
            let attributes = reader.u8()?;
            let expected = u8::from(class == 12 || class == 13)
                | (u8::from(class == 11) << 1)
                | (u8::from((unit - 0xac00) % 28 != 0) << 2);
            if class as usize >= CLASSES.len() || attributes != expected {
                return Err(reader.invalid("invalid character class or morphology attributes"));
            }
            values.push([class, attributes]);
        }
        Ok(Self {
            flags,
            words,
            values,
        })
    }

    #[cfg(any(test, feature = "nori-tools"))]
    pub fn encode(&self, output: &mut crate::nori::io::Writer) -> DictionaryResult<()> {
        output.count(CLASSES.len())?;
        output.bytes(&self.flags)?;
        for words in &self.words {
            output.u32(words.start)?;
            output.u32(words.end - words.start)?;
        }
        output.count(self.values.len())?;
        for value in &self.values {
            output.bytes(value)?;
        }
        Ok(())
    }
}
