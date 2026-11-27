//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct every neutral-model value from the decoded runtime representation.

use crate::nori::dictionary::tables::CLASSES;
use crate::nori::{DictionaryLimits, DictionaryResult, DictionaryWord, NoriDictionary};

use crate::morphology::neutral::{self, hash::NeutralHash, verify::NeutralFiles};

fn write_word(output: &mut NeutralHash, word: DictionaryWord<'_>) -> neutral::Result<()> {
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

/// Reconstruct all five neutral streams and compare their complete hashes, including every lexicon lookup.
pub fn verify_dictionary(
    dictionary: &NoriDictionary,
    limits: DictionaryLimits,
) -> DictionaryResult<()> {
    let files = NeutralFiles::new(dictionary.provenance(), *b"UQAN", limits)?;
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
                dictionary.character_morphology_flags(unit),
            ]
        },
    )?;
    files.unicode(&dictionary.unicode)?;
    Ok(())
}
