//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct every neutral-model value from the decoded runtime representation.

use crate::kuromoji::error::{check_limit, invalid};
use crate::kuromoji::tables::CLASSES;
use crate::kuromoji::{DictionaryLimits, DictionaryResult, DictionaryWord, KuromojiDictionary};

use crate::morphology::neutral::hash::NeutralHash;

fn write_word(output: &mut NeutralHash, word: DictionaryWord<'_>) -> DictionaryResult<()> {
    output.u32(word.original_id())?;
    output.u32(u32::from(word.left_context()))?;
    output.u32(u32::from(word.right_context()))?;
    output.i32(i32::from(word.cost()))?;
    output.text(Some(word.part_of_speech()))?;
    output.text(word.base_form())?;
    output.text(word.reading())?;
    output.text(word.pronunciation())?;
    output.text(word.inflection_type())?;
    output.text(word.inflection_form())?;
    Ok(())
}

fn verify(
    dictionary: &KuromojiDictionary,
    name: &str,
    write: impl FnOnce(&mut NeutralHash) -> DictionaryResult<()>,
) -> DictionaryResult<()> {
    let mut output = NeutralHash::new(dictionary.provenance(), name)?;
    write(&mut output)?;
    output.finish().map_err(Into::into)
}

/// Reconstruct all six neutral streams and compare their complete hashes, including every lexicon lookup.
pub fn verify_dictionary(
    dictionary: &KuromojiDictionary,
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
        output.bytes(b"UQAJCCS1")?;
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
        output.bytes(b"UQAJCHR1")?;
        output.count(CLASSES.len())?;
        output.bytes(&dictionary.characters.flags)?;
        output.count(dictionary.characters.values.len())?;
        for unit in 0..0x1_0000 {
            output.bytes(&[
                dictionary.character_class(unit as u16),
                u8::from(dictionary.is_kanji(unit as u16)),
            ])?;
        }
        Ok(())
    })?;
    verify(dictionary, "unicode.bin", |output| {
        output.bytes(b"UQAJUNI1")?;
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
    verify(dictionary, "analysis.bin", |output| {
        output.bytes(b"UQAJANA1")?;
        output.count(dictionary.analysis.stop_words.len())?;
        for word in &dictionary.analysis.stop_words {
            output.text(Some(word))?;
        }
        output.count(dictionary.analysis.stop_tags.len())?;
        for tag in &dictionary.analysis.stop_tags {
            output.text(Some(tag))?;
        }
        output.count(dictionary.analysis.completion.len())?;
        for mapping in &dictionary.analysis.completion {
            output.text(Some(&mapping.key))?;
            output.count(mapping.alternatives.len())?;
            for alternative in &mapping.alternatives {
                output.text(Some(alternative))?;
            }
        }
        Ok(())
    })?;
    Ok(())
}

fn verify_lexicon(dictionary: &KuromojiDictionary) -> DictionaryResult<()> {
    verify(dictionary, "lexicon.bin", |output| {
        output.bytes(b"UQAJLEX1")?;
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

fn verify_unknown(dictionary: &KuromojiDictionary) -> DictionaryResult<()> {
    verify(dictionary, "unknown.bin", |output| {
        output.bytes(b"UQAJUNK1")?;
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
