//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct every neutral-model value from the decoded runtime representation.

use crate::nori::dictionary::tables::CLASSES;
use crate::nori::error::{check_limit, invalid};
use crate::nori::{DictionaryLimits, DictionaryResult, DictionaryWord, NoriDictionary};

use crate::morphology::neutral::hash::NeutralHash;

fn write_word(output: &mut NeutralHash, word: DictionaryWord<'_>) -> DictionaryResult<()> {
    output.u32(word.original_id())?;
    output.u32(u32::from(word.left_context()))?;
    output.u32(u32::from(word.right_context()))?;
    output.i32(i32::from(word.cost()))?;
    output.u32(word.pos_type() as u32)?;
    output.u32(u32::from(word.left_pos().ordinal()))?;
    output.u32(u32::from(word.right_pos().ordinal()))?;
    output.text(word.reading())?;
    if let Some(morphemes) = word.morphemes() {
        output.count(morphemes.len())?;
        for morpheme in morphemes {
            output.u32(u32::from(morpheme.pos.ordinal()))?;
            output.text(Some(morpheme.surface))?;
        }
    } else {
        output.i32(-1)?;
    }
    Ok(())
}

fn verify(
    dictionary: &NoriDictionary,
    name: &str,
    write: impl FnOnce(&mut NeutralHash) -> DictionaryResult<()>,
) -> DictionaryResult<()> {
    let mut output = NeutralHash::new(dictionary.provenance(), name)?;
    write(&mut output)?;
    output.finish().map_err(Into::into)
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
        output.u32(crate::morphology::unicode::CODE_POINTS)?;
        for code_point in 0..crate::morphology::unicode::CODE_POINTS {
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
                write_word(
                    output,
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
                write_word(
                    output,
                    dictionary
                        .word(id)
                        .ok_or_else(|| invalid("neutral verification", "missing unknown word"))?,
                )?;
            }
        }
        Ok(())
    })
}
