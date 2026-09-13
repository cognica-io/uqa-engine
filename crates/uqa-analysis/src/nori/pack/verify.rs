//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct every neutral-model value from the decoded runtime representation.

use sha2::{Digest, Sha256};

use crate::nori::dictionary::tables::CLASSES;
use crate::nori::error::{check_limit, invalid};
use crate::nori::{DictionaryLimits, DictionaryResult, DictionaryWord, NoriDictionary};

struct NeutralHash {
    hash: Sha256,
    bytes: u64,
    expected_bytes: u64,
}

impl NeutralHash {
    fn bytes(&mut self, bytes: &[u8]) -> DictionaryResult<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("neutral verification", "byte count overflow"))?;
        if self.bytes > self.expected_bytes {
            return Err(invalid(
                "neutral verification",
                "reconstructed data exceeds expected length",
            ));
        }
        self.hash.update(bytes);
        Ok(())
    }

    fn u32(&mut self, value: u32) -> DictionaryResult<()> {
        self.bytes(&value.to_be_bytes())
    }
    fn i32(&mut self, value: i32) -> DictionaryResult<()> {
        self.u32(value as u32)
    }
    fn u16(&mut self, value: u16) -> DictionaryResult<()> {
        self.bytes(&value.to_be_bytes())
    }

    fn count(&mut self, value: usize) -> DictionaryResult<()> {
        self.u32(
            u32::try_from(value)
                .map_err(|_| invalid("neutral verification", "count exceeds u32"))?,
        )
    }

    fn text(&mut self, value: Option<&str>) -> DictionaryResult<()> {
        let Some(text) = value else {
            return self.i32(-1);
        };
        self.count(text.encode_utf16().count())?;
        for unit in text.encode_utf16() {
            self.u16(unit)?;
        }
        Ok(())
    }

    fn word(&mut self, word: DictionaryWord<'_>) -> DictionaryResult<()> {
        self.u32(word.original_id())?;
        self.u32(u32::from(word.left_context()))?;
        self.u32(u32::from(word.right_context()))?;
        self.i32(i32::from(word.cost()))?;
        self.u32(word.pos_type() as u32)?;
        self.u32(u32::from(word.left_pos().ordinal()))?;
        self.u32(u32::from(word.right_pos().ordinal()))?;
        self.text(word.reading())?;
        if let Some(morphemes) = word.morphemes() {
            self.count(morphemes.len())?;
            for morpheme in morphemes {
                self.u32(u32::from(morpheme.pos.ordinal()))?;
                self.text(Some(morpheme.surface))?;
            }
        } else {
            self.i32(-1)?;
        }
        Ok(())
    }
}

fn verify(
    dictionary: &NoriDictionary,
    name: &str,
    write: impl FnOnce(&mut NeutralHash) -> DictionaryResult<()>,
) -> DictionaryResult<()> {
    let files = dictionary.provenance()["files"]
        .as_array()
        .ok_or_else(|| invalid("neutral verification", "missing file inventory"))?;
    let expected = files
        .iter()
        .find(|file| file["path"] == name)
        .ok_or_else(|| invalid("neutral verification", "missing expected file"))?;
    let expected_bytes = expected["bytes"]
        .as_u64()
        .ok_or_else(|| invalid("neutral verification", "missing expected byte count"))?;
    let mut output = NeutralHash {
        hash: Sha256::new(),
        bytes: 0,
        expected_bytes,
    };
    write(&mut output)?;
    let digest = format!("{:x}", output.hash.finalize());
    if output.bytes != expected_bytes || expected["sha256"] != digest {
        return Err(invalid(
            "neutral verification",
            "reconstructed model hash or length differs",
        ));
    }
    Ok(())
}

/// Reconstruct all five neutral streams and compare their complete hashes, including every lexicon lookup.
pub fn verify_dictionary(
    dictionary: &NoriDictionary,
    limits: DictionaryLimits,
) -> DictionaryResult<()> {
    let files = dictionary.provenance()["files"]
        .as_array()
        .ok_or_else(|| invalid("neutral verification", "missing file inventory"))?;
    let mut total = 0_usize;
    for file in files {
        let count = file["bytes"]
            .as_u64()
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| invalid("neutral verification", "invalid expected byte count"))?;
        total = total
            .checked_add(count)
            .ok_or_else(|| invalid("neutral verification", "size overflow"))?;
    }
    check_limit(
        "reconstructed neutral bytes",
        total,
        limits.max_decoded_bytes,
    )?;
    verify_lexicon(dictionary)?;
    verify_unknown(dictionary)?;
    verify(dictionary, "connection_costs.bin", |output| {
        output.bytes(b"UQANCCS1")?;
        output.count(dictionary.matrix.forward)?;
        output.count(dictionary.matrix.backward)?;
        for backward in 0..dictionary.matrix.backward {
            for forward in 0..dictionary.matrix.forward {
                let cost = dictionary
                    .matrix
                    .get(forward, backward)
                    .ok_or_else(|| invalid("neutral verification", "missing connection cost"))?;
                output.u16(cost as u16)?;
            }
        }
        Ok(())
    })?;
    verify(dictionary, "characters.bin", |output| {
        output.bytes(b"UQANCHR1")?;
        output.count(CLASSES.len())?;
        output.bytes(&dictionary.characters.flags)?;
        output.count(dictionary.characters.values.len())?;
        for unit in 0..0x1_0000 {
            output.bytes(&[
                dictionary.character_class(unit as u16),
                dictionary.character_morphology_flags(unit as u16),
            ])?;
        }
        Ok(())
    })?;
    verify(dictionary, "unicode.bin", |output| {
        output.bytes(b"UQANUNI1")?;
        output.u32(crate::nori::unicode::CODE_POINTS)?;
        for code_point in 0..crate::nori::unicode::CODE_POINTS {
            let value = dictionary
                .unicode(code_point)
                .ok_or_else(|| invalid("neutral verification", "missing code point"))?;
            output.bytes(&[value.category])?;
            output.u16(value.script)?;
            output.bytes(&[u8::from(value.is_digit)
                | (u8::from(value.is_whitespace) << 1)
                | (u8::from(value.is_space_char) << 2)])?;
            output.u32(value.lowercase)?;
        }
        Ok(())
    })?;
    Ok(())
}

fn verify_lexicon(dictionary: &NoriDictionary) -> DictionaryResult<()> {
    verify(dictionary, "lexicon.bin", |output| {
        output.bytes(b"UQANLEX1")?;
        output.count(dictionary.surfaces.len())?;
        output.u32(dictionary.known_words)?;
        for (index, units) in dictionary.lexicon.entries().enumerate() {
            let units = units?;
            if dictionary.lexicon.lookup(units.iter().copied()) != Some(index as u32) {
                return Err(invalid(
                    "neutral verification",
                    "enumerated and looked-up ranks differ",
                ));
            }
            let text = String::from_utf16(&units)?;
            let surface = &dictionary.surfaces[index];
            output.u32(surface.source_id)?;
            output.text(Some(&text))?;
            output.u32(surface.word_ids.end - surface.word_ids.start)?;
            for id in surface.word_ids.clone() {
                output.word(
                    dictionary
                        .word(id)
                        .ok_or_else(|| invalid("neutral verification", "missing system word"))?,
                )?;
            }
        }
        Ok(())
    })
}

fn verify_unknown(dictionary: &NoriDictionary) -> DictionaryResult<()> {
    verify(dictionary, "unknown.bin", |output| {
        output.bytes(b"UQANUNK1")?;
        output.count(CLASSES.len())?;
        output.count(dictionary.words.len() - dictionary.known_words as usize)?;
        for (index, name) in CLASSES.iter().enumerate() {
            output.count(index)?;
            output.text(Some(name))?;
            let words = &dictionary.characters.words[index];
            output.u32(words.end - words.start)?;
            for id in words.clone() {
                output.word(
                    dictionary
                        .word(id)
                        .ok_or_else(|| invalid("neutral verification", "missing unknown word"))?,
                )?;
            }
        }
        Ok(())
    })
}
