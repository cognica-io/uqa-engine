//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Case mapping through the provider of a call's input collation, as `PostgreSQL` 18's `str_tolower`, `str_toupper`, `str_initcap` and `str_casefold` select it (`formatting.c`, `pg_locale_builtin.c`, `pg_locale_icu.c`). Mappings admit their destination before writing into it. Every provider maps ASCII letters alike, so ASCII runs map bytewise; non-ASCII runs map as whole strings where the provider's mapping is context-free. Both cancellation scopes are observed once per bounded chunk of input.

mod characters;
mod output;
mod word_breaks;

use crate::catalog::collations::{builtin_collation_ctype, BuiltinCollationCtype};
use crate::SQLError;
use characters::{CaseKind, FinalSigmaRule, UnicodeCharacters};
use output::MappedOutput;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

/// Input bytes mapped between cancellation checks.
const CHUNK: usize = 4096;

/// ICU words titlecased between cancellation checks.
const WORDS: usize = 256;

/// The case-mapping behavior that a determinate input collation selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::expr) struct CaseMapping {
    ctype: BuiltinCollationCtype,
}

impl CaseMapping {
    /// Select the provider of `collation`, which is `PostgreSQL`'s `inputcollid`. An indeterminate collation (zero) reports the function-specific `42P22` diagnostic that `subject` names, such as `lower() function` or `ILIKE`.
    pub(in crate::expr) fn select(collation: u32, subject: &str) -> Result<Self, SQLError> {
        if collation == 0 {
            return Err(SQLError::Diagnostic {
                sqlstate: "42P22".into(),
                message: format!("could not determine which collation to use for {subject}"),
                detail: None,
                hint: Some("Use the COLLATE clause to set the collation explicitly.".into()),
            });
        }
        let ctype = builtin_collation_ctype(collation).ok_or_else(|| {
            SQLError::Internal(format!("cache lookup failed for collation {collation}"))
        })?;
        Ok(Self { ctype })
    }

    pub(in crate::expr) fn lowercase(
        self,
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        self.map(text, CaseKind::Lower, control)
    }

    pub(in crate::expr) fn uppercase(
        self,
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        self.map(text, CaseKind::Upper, control)
    }

    /// `casefold`; a C-type collation folds like `asc_tolower`.
    pub(in crate::expr) fn fold(
        self,
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        self.map(text, CaseKind::Fold, control)
    }

    /// `initcap`: word starts map to titlecase (uppercase under simple mappings) and the rest of each word to lowercase. Words follow ASCII alphanumerics under a C type, Unicode alphanumerics under the builtin provider, and ICU's root word boundaries under ICU.
    pub(in crate::expr) fn titlecase(
        self,
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        let mut output = ProductionString::new(*control);
        control.check()?;
        output.reserve(text.len())?;
        let mut mapped = MappedOutput::new(&mut output);
        match self.ctype {
            BuiltinCollationCtype::Ascii => {
                let mut previous_alphanumeric = false;
                for (start, end) in chunks(text) {
                    control.check()?;
                    for character in text[start..end].chars() {
                        let mapped_character = if previous_alphanumeric {
                            character.to_ascii_lowercase()
                        } else {
                            character.to_ascii_uppercase()
                        };
                        mapped.push(mapped_character)?;
                        previous_alphanumeric = mapped_character.is_ascii_alphanumeric();
                    }
                }
            }
            BuiltinCollationCtype::UnicodeSimple | BuiltinCollationCtype::UnicodeFull => {
                let full = self.ctype == BuiltinCollationCtype::UnicodeFull;
                let characters = UnicodeCharacters::new(FinalSigmaRule::Builtin);
                let mut previous = None;
                for (start, end) in chunks(text) {
                    control.check()?;
                    for (offset, character) in text[start..end].char_indices() {
                        // ASCII letters and digits are alphanumeric under both builtin locales, and their titlecase is their uppercase.
                        let alphanumeric = if character.is_ascii() {
                            character.is_ascii_alphanumeric()
                        } else {
                            characters.is_alphanumeric(character, !full)
                        };
                        let start_of_word = previous != Some(alphanumeric);
                        previous = Some(alphanumeric);
                        if character.is_ascii() {
                            mapped.push(if start_of_word {
                                character.to_ascii_uppercase()
                            } else {
                                character.to_ascii_lowercase()
                            })?;
                            continue;
                        }
                        let kind = match (start_of_word, full) {
                            (false, _) => CaseKind::Lower,
                            (true, true) => CaseKind::Title,
                            (true, false) => CaseKind::Upper,
                        };
                        if full {
                            characters.push_full(
                                text,
                                start + offset,
                                character,
                                kind,
                                &mut mapped,
                                control,
                            )?;
                        } else {
                            mapped.push(characters.simple(character, kind))?;
                        }
                    }
                }
            }
            BuiltinCollationCtype::IcuRoot => icu_titlecase(text, &mut mapped, control)?,
        }
        mapped.flush()?;
        output.finish()
    }

    fn map(
        self,
        text: &str,
        kind: CaseKind,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        let mut output = ProductionString::new(*control);
        control.check()?;
        output.reserve(text.len())?;
        let mut mapped = MappedOutput::new(&mut output);
        let characters = UnicodeCharacters::new(self.sigma_rule());
        for (start, end) in chunks(text) {
            control.check()?;
            self.map_range(text, start, end, kind, characters, &mut mapped, control)?;
        }
        mapped.flush()?;
        output.finish()
    }

    fn sigma_rule(self) -> FinalSigmaRule {
        if self.ctype == BuiltinCollationCtype::IcuRoot {
            FinalSigmaRule::Icu
        } else {
            FinalSigmaRule::Builtin
        }
    }

    /// Map `text[start..end]`, which begins and ends on character boundaries, alternating ASCII runs, which every provider maps alike, with non-ASCII runs. A context-dependent mapping still reads the whole text.
    #[expect(
        clippy::too_many_arguments,
        reason = "a range of the whole text keeps the context that a capital sigma's lowercase reads"
    )]
    fn map_range(
        self,
        text: &str,
        start: usize,
        end: usize,
        kind: CaseKind,
        characters: UnicodeCharacters,
        mapped: &mut MappedOutput<'_, '_>,
        control: &ProductionControl<'_>,
    ) -> Result<(), ValueRetentionError> {
        let bytes = text.as_bytes();
        let mut position = start;
        while position < end {
            let ascii = bytes[position..end]
                .iter()
                .take_while(|byte| byte.is_ascii())
                .count();
            if ascii > 0 {
                mapped.push_ascii(&text[position..position + ascii], kind == CaseKind::Upper)?;
                position += ascii;
                continue;
            }
            // A non-ASCII character encodes only bytes outside ASCII, so the next ASCII byte starts a character.
            let run_end = bytes[position..end]
                .iter()
                .position(u8::is_ascii)
                .map_or(end, |length| position + length);
            match self.ctype {
                BuiltinCollationCtype::Ascii => mapped.push_str(&text[position..run_end])?,
                BuiltinCollationCtype::UnicodeSimple => {
                    for character in text[position..run_end].chars() {
                        mapped.push(characters.simple(character, kind))?;
                    }
                }
                BuiltinCollationCtype::UnicodeFull | BuiltinCollationCtype::IcuRoot => {
                    characters.push_full_run(text, position, run_end, kind, mapped, control)?;
                }
            }
            position = run_end;
        }
        Ok(())
    }
}

/// Consecutive ranges of at most [`CHUNK`] bytes that begin and end on character boundaries.
fn chunks(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        (start < text.len()).then(|| {
            let mut end = (start + CHUNK).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let range = (start, end);
            start = end;
            range
        })
    })
}

/// ICU's `ustrcase_internalToTitle` with its default break adjustment: in each word, characters before the first letter, number, symbol, private-use character or cased modifier letter are copied, that character maps to titlecase, and the rest of the word maps to lowercase in the context of the whole string.
fn icu_titlecase(
    text: &str,
    mapped: &mut MappedOutput<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    let characters = UnicodeCharacters::new(FinalSigmaRule::Icu);
    let icu = CaseMapping {
        ctype: BuiltinCollationCtype::IcuRoot,
    };
    let boundaries = word_breaks::boundaries(text, control)?;
    for (index, segment) in boundaries.windows(2).enumerate() {
        // Words are observed in batches; a long word observes cancellation as its mapped output is admitted.
        if index.is_multiple_of(WORDS) {
            control.check()?;
        }
        let (start, end) = (segment[0], segment[1]);
        let Some((offset, character)) = text[start..end]
            .char_indices()
            .find(|(_, character)| characters.is_title_start(*character))
        else {
            mapped.push_str(&text[start..end])?;
            continue;
        };
        let title = start + offset;
        mapped.push_str(&text[start..title])?;
        characters.push_full(text, title, character, CaseKind::Title, mapped, control)?;
        icu.map_range(
            text,
            title + character.len_utf8(),
            end,
            CaseKind::Lower,
            characters,
            mapped,
            control,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
