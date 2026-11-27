//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct every neutral-model value from the decoded runtime representation.

use crate::kuromoji::tables::CLASSES;
use crate::kuromoji::{DictionaryLimits, DictionaryResult, DictionaryWord, KuromojiDictionary};

use crate::morphology::neutral::{self, hash::NeutralHash, verify::NeutralFiles};

fn write_word(output: &mut NeutralHash, word: DictionaryWord<'_>) -> neutral::Result<()> {
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

/// Reconstruct all six neutral streams and compare their complete hashes, including every lexicon lookup.
pub fn verify_dictionary(
    dictionary: &KuromojiDictionary,
    limits: DictionaryLimits,
) -> DictionaryResult<()> {
    let files = NeutralFiles::new(dictionary.provenance(), *b"UQAJ", limits)?;
    let word = |id| dictionary.word(id);
    files.lexicon(
        &dictionary.lexicon,
        &dictionary.surfaces,
        dictionary.known_words,
        word,
        write_word,
    )?;
    files.unknown(
        CLASSES,
        &dictionary.characters.words,
        dictionary.words.len() - dictionary.known_words as usize,
        word,
        write_word,
    )?;
    files.matrix(&dictionary.matrix)?;
    files.characters(
        CLASSES.len(),
        &dictionary.characters.flags,
        dictionary.characters.values.len(),
        |unit| {
            [
                dictionary.character_class(unit),
                u8::from(dictionary.is_kanji(unit)),
            ]
        },
    )?;
    files.unicode(&dictionary.unicode)?;
    files.stream("analysis.bin", *b"ANA1", |output| {
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
