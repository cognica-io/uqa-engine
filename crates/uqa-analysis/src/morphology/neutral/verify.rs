//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct the neutral streams shared by the Korean and Japanese dictionary formats.

use super::hash::NeutralHash;
use super::Result;
use crate::morphology::{
    error::{check_limit, invalid},
    lexicon::Lexicon,
    limits::DictionaryLimits,
    matrix::Matrix,
    surfaces::SurfaceWords,
    unicode::{UnicodeTable, CODE_POINTS},
};
use serde_json::Value;
use std::ops::Range;

pub(crate) struct NeutralFiles<'a> {
    manifest: &'a Value,
    prefix: [u8; 4],
}

impl<'a> NeutralFiles<'a> {
    pub(crate) fn new(
        manifest: &'a Value,
        prefix: [u8; 4],
        limits: DictionaryLimits,
    ) -> Result<Self> {
        let files = manifest["files"]
            .as_array()
            .ok_or_else(|| invalid("neutral verification", "missing file inventory"))?;
        let mut total = 0usize;
        for file in files {
            let bytes = file["bytes"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| invalid("neutral verification", "invalid expected byte count"))?;
            total = total
                .checked_add(bytes)
                .ok_or_else(|| invalid("neutral verification", "size overflow"))?;
        }
        check_limit(
            "reconstructed neutral bytes",
            total,
            limits.max_decoded_bytes,
        )?;
        Ok(Self { manifest, prefix })
    }

    pub(crate) fn stream(
        &self,
        name: &str,
        suffix: [u8; 4],
        write: impl FnOnce(&mut NeutralHash) -> Result<()>,
    ) -> Result<()> {
        let mut output = NeutralHash::new(self.manifest, name)?;
        let mut magic = [0; 8];
        magic[..4].copy_from_slice(&self.prefix);
        magic[4..].copy_from_slice(&suffix);
        output.bytes(&magic)?;
        write(&mut output)?;
        output.finish()
    }

    pub(crate) fn lexicon<W>(
        &self,
        lexicon: &Lexicon,
        surfaces: &[SurfaceWords],
        known_words: u32,
        word: impl Fn(u32) -> Option<W>,
        write_word: impl Fn(&mut NeutralHash, W) -> Result<()>,
    ) -> Result<()> {
        self.stream("lexicon.bin", *b"LEX1", |output| {
            output.count(surfaces.len())?;
            output.u32(known_words)?;
            for (index, units) in lexicon.entries().enumerate() {
                let units = units?;
                if lexicon.lookup(units.iter().copied()) != Some(index as u32) {
                    return Err(invalid(
                        "neutral verification",
                        "enumerated and looked-up ranks differ",
                    )
                    .into());
                }
                let text = String::from_utf16(&units)?;
                let surface = &surfaces[index];
                output.u32(surface.source_id)?;
                output.text(Some(&text))?;
                output.u32(surface.word_ids.end - surface.word_ids.start)?;
                for id in surface.word_ids.clone() {
                    write_word(
                        output,
                        word(id).ok_or_else(|| {
                            invalid("neutral verification", "missing system word")
                        })?,
                    )?;
                }
            }
            Ok(())
        })
    }

    pub(crate) fn unknown<W>(
        &self,
        classes: &[&str],
        ranges: &[Range<u32>],
        count: usize,
        word: impl Fn(u32) -> Option<W>,
        write_word: impl Fn(&mut NeutralHash, W) -> Result<()>,
    ) -> Result<()> {
        self.stream("unknown.bin", *b"UNK1", |output| {
            output.count(classes.len())?;
            output.count(count)?;
            for (index, name) in classes.iter().enumerate() {
                output.count(index)?;
                output.text(Some(name))?;
                let words = &ranges[index];
                output.u32(words.end - words.start)?;
                for id in words.clone() {
                    write_word(
                        output,
                        word(id).ok_or_else(|| {
                            invalid("neutral verification", "missing unknown word")
                        })?,
                    )?;
                }
            }
            Ok(())
        })
    }

    pub(crate) fn matrix(&self, matrix: &Matrix) -> Result<()> {
        self.stream("connection_costs.bin", *b"CCS1", |output| {
            output.count(matrix.forward)?;
            output.count(matrix.backward)?;
            for backward in 0..matrix.backward {
                for forward in 0..matrix.forward {
                    let cost = matrix.get(forward, backward).ok_or_else(|| {
                        invalid("neutral verification", "missing connection cost")
                    })?;
                    output.u16(cost as u16)?;
                }
            }
            Ok(())
        })
    }

    pub(crate) fn characters(
        &self,
        classes: usize,
        flags: &[u8],
        values: usize,
        attributes: impl Fn(u16) -> [u8; 2],
    ) -> Result<()> {
        self.stream("characters.bin", *b"CHR1", |output| {
            output.count(classes)?;
            output.bytes(flags)?;
            output.count(values)?;
            for unit in 0..=u16::MAX {
                output.bytes(&attributes(unit))?;
            }
            Ok(())
        })
    }

    pub(crate) fn unicode(&self, table: &UnicodeTable) -> Result<()> {
        self.stream("unicode.bin", *b"UNI1", |output| {
            output.u32(CODE_POINTS)?;
            for code_point in 0..CODE_POINTS {
                let value = table
                    .get(code_point)
                    .ok_or_else(|| invalid("neutral verification", "missing code point"))?;
                output.bytes(&[value.category])?;
                output.u16(value.script)?;
                output.bytes(&[u8::from(value.is_digit)
                    | (u8::from(value.is_whitespace) << 1)
                    | (u8::from(value.is_space_char) << 2)])?;
                output.u32(value.lowercase)?;
            }
            Ok(())
        })
    }
}
