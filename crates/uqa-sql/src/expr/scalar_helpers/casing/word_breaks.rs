//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Word boundaries of ICU 76's root break rules (`icu4c/source/data/brkitr/rules/word.txt`), which ICU's `u_strToTitle` uses when `PostgreSQL` titlecases under an ICU collation.
//!
//! The rules are UAX #29 with ICU's tailoring: colons are not `MidLetter`, Han characters are never `Extend`, complex-context letters join `ALetter`, and Hangul syllables, Han, Hiragana and Katakana chain only with their immediate neighbours of the same group. ICU additionally subdivides runs of the dictionary scripts with its dictionaries. A subdivision starts a segment at an uncased character, so it leaves the titlecased result unchanged unless it falls before uncased modifier letters, such as Thai MAIYAMOK, that a cased letter follows directly in the same segment; these rules do not reproduce that subdivision.

use icu_properties::{
    props::{ExtendedPictographic, GraphemeClusterBreak, LineBreak, Script, WordBreak as Property},
    CodePointMapData, CodePointSetData,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueRetentionError,
};

/// A character's role in the rules. Complex-context letters are `ALetter`, and `ALetter` excludes the dictionary CJK scripts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Cr,
    Lf,
    Newline,
    Extend,
    Format,
    Zwj,
    RegionalIndicator,
    Katakana,
    HebrewLetter,
    ALetter,
    SingleQuote,
    DoubleQuote,
    MidNumLet,
    MidLetter,
    MidNum,
    Numeric,
    ExtendNumLet,
    WSegSpace,
    Other,
}

#[derive(Clone, Copy)]
struct Character {
    class: Class,
    pictographic: bool,
    hangul_syllable: bool,
    kana_kanji: bool,
}

impl Character {
    fn new(character: char) -> Self {
        let word_break = CodePointMapData::<Property>::new().get(character);
        let script = CodePointMapData::<Script>::new().get(character);
        let han = script == Script::Han;
        let kana_kanji = han || script == Script::Hiragana || word_break == Property::Katakana;
        let hangul_syllable = ('\u{AC00}'..='\u{D7A3}').contains(&character);
        let mut class = match word_break {
            Property::CR => Class::Cr,
            Property::LF => Class::Lf,
            Property::Newline => Class::Newline,
            Property::Extend if !han => Class::Extend,
            Property::Format => Class::Format,
            Property::ZWJ => Class::Zwj,
            Property::RegionalIndicator => Class::RegionalIndicator,
            Property::Katakana => Class::Katakana,
            Property::HebrewLetter => Class::HebrewLetter,
            Property::ALetter if !kana_kanji && !hangul_syllable => Class::ALetter,
            Property::SingleQuote => Class::SingleQuote,
            Property::DoubleQuote => Class::DoubleQuote,
            Property::MidNumLet => Class::MidNumLet,
            Property::MidLetter if !matches!(character, ':' | '\u{FE55}' | '\u{FF1A}') => {
                Class::MidLetter
            }
            Property::MidNum => Class::MidNum,
            Property::Numeric => Class::Numeric,
            Property::ExtendNumLet => Class::ExtendNumLet,
            Property::WSegSpace => Class::WSegSpace,
            _ => Class::Other,
        };
        if class == Class::Other
            && CodePointMapData::<LineBreak>::new().get(character) == LineBreak::ComplexContext
            && CodePointMapData::<GraphemeClusterBreak>::new().get(character)
                != GraphemeClusterBreak::Control
        {
            class = Class::ALetter;
        }
        Self {
            class,
            pictographic: CodePointSetData::new::<ExtendedPictographic>().contains(character),
            hangul_syllable,
            kana_kanji,
        }
    }

    /// `$ExFm`: characters that attach to the preceding character.
    fn attaches(self) -> bool {
        matches!(self.class, Class::Extend | Class::Format | Class::Zwj)
    }

    fn letter(self) -> bool {
        matches!(self.class, Class::ALetter | Class::HebrewLetter)
    }

    fn mid_letter(self) -> bool {
        matches!(
            self.class,
            Class::MidLetter | Class::MidNumLet | Class::SingleQuote
        )
    }

    fn mid_number(self) -> bool {
        matches!(
            self.class,
            Class::MidNum | Class::MidNumLet | Class::SingleQuote
        )
    }

    fn line_end(self) -> bool {
        matches!(self.class, Class::Cr | Class::Lf | Class::Newline)
    }
}

/// The ordered byte offsets where `text` has a word boundary, including its start and end.
pub(super) fn boundaries(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, ValueRetentionError> {
    let mut characters = ProductionVec::new(*control);
    for character in text.chars() {
        control.check()?;
        characters.push_copy(Character::new(character))?;
    }
    let mut offsets = ProductionVec::new(*control);
    offsets.push_copy(0)?;
    let mut offset = 0;
    for (index, character) in text.chars().enumerate() {
        control.check()?;
        if index > 0 && !joins(&characters, index) {
            offsets.push_copy(offset)?;
        }
        offset += character.len_utf8();
    }
    if !text.is_empty() {
        offsets.push_copy(text.len())?;
    }
    offsets.finish()
}

/// Whether the characters before and at `index` stay in one segment.
fn joins(characters: &[Character], index: usize) -> bool {
    let previous = characters[index - 1];
    let next = characters[index];
    if previous.class == Class::Cr && next.class == Class::Lf {
        return true;
    }
    if previous.line_end() || next.line_end() {
        return false;
    }
    if (previous.class == Class::Zwj && next.pictographic)
        || (previous.class == Class::WSegSpace && next.class == Class::WSegSpace)
        || next.attaches()
        || (previous.hangul_syllable && next.hangul_syllable)
        || (previous.kana_kanji && next.kana_kanji)
    {
        return true;
    }
    let Some(left) = base_before(characters, index) else {
        return false;
    };
    let base = characters[left];
    let before = || base_before(characters, left).map(|index| characters[index]);
    let after = || base_after(characters, index).map(|index| characters[index]);
    if base.letter() && next.letter()
        || base.letter() && next.mid_letter() && after().is_some_and(Character::letter)
        || base.mid_letter() && next.letter() && before().is_some_and(Character::letter)
        || base.class == Class::HebrewLetter && next.class == Class::SingleQuote
    {
        return true;
    }
    let hebrew = |character: Character| character.class == Class::HebrewLetter;
    let numeric = |character: Character| character.class == Class::Numeric;
    if base.class == Class::HebrewLetter
        && next.class == Class::DoubleQuote
        && after().is_some_and(hebrew)
        || base.class == Class::DoubleQuote && hebrew(next) && before().is_some_and(hebrew)
        || numeric(base) && numeric(next)
        || base.letter() && numeric(next)
        || numeric(base) && next.letter()
        || base.mid_number() && numeric(next) && before().is_some_and(numeric)
        || numeric(base) && next.mid_number() && after().is_some_and(numeric)
        || base.class == Class::Katakana && next.class == Class::Katakana
    {
        return true;
    }
    if next.class == Class::ExtendNumLet
        && (base.letter()
            || matches!(
                base.class,
                Class::Numeric | Class::Katakana | Class::ExtendNumLet
            ))
        || base.class == Class::ExtendNumLet
            && (next.letter() || matches!(next.class, Class::Numeric | Class::Katakana))
    {
        return true;
    }
    if base.class == Class::RegionalIndicator && next.class == Class::RegionalIndicator {
        // Indicators pair from the start of their run; an odd count before this one leaves it unpaired.
        let mut count = 0_usize;
        let mut cursor = Some(left);
        while let Some(position) = cursor {
            if characters[position].class != Class::RegionalIndicator {
                break;
            }
            count += 1;
            cursor = base_before(characters, position);
        }
        return count % 2 == 1;
    }
    false
}

/// The nearest character before `index` that does not attach to its predecessor. Attached characters after the start of text or a line end are not part of a preceding base.
fn base_before(characters: &[Character], index: usize) -> Option<usize> {
    let mut position = index;
    while position > 0 {
        position -= 1;
        let character = characters[position];
        if !character.attaches() {
            return (!(character.line_end() && position + 1 < index)).then_some(position);
        }
    }
    None
}

/// The nearest character after `index` that does not attach to its predecessor.
fn base_after(characters: &[Character], index: usize) -> Option<usize> {
    (index + 1..characters.len()).find(|position| !characters[*position].attaches())
}

#[cfg(test)]
mod tests;
