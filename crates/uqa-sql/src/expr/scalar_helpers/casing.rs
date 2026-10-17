//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Case mapping through the provider of a call's input collation, as `PostgreSQL` 18's `str_tolower`, `str_toupper`, `str_initcap` and `str_casefold` select it (`formatting.c`, `pg_locale_builtin.c`, `pg_locale_icu.c`). Mappings write directly into their admitted destination.

mod characters;
mod word_breaks;

use crate::catalog::collations::{builtin_collation_ctype, BuiltinCollationCtype};
use crate::SQLError;
use characters::{CaseKind, FinalSigmaRule, UnicodeCharacters};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

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
        match self.ctype {
            BuiltinCollationCtype::Ascii => {
                let mut previous_alphanumeric = false;
                for character in text.chars() {
                    control.check()?;
                    let mapped = if previous_alphanumeric {
                        character.to_ascii_lowercase()
                    } else {
                        character.to_ascii_uppercase()
                    };
                    output.push(mapped)?;
                    previous_alphanumeric = mapped.is_ascii_alphanumeric();
                }
            }
            BuiltinCollationCtype::UnicodeSimple | BuiltinCollationCtype::UnicodeFull => {
                let full = self.ctype == BuiltinCollationCtype::UnicodeFull;
                let characters = UnicodeCharacters::new(FinalSigmaRule::Builtin);
                let mut previous = None;
                for (offset, character) in text.char_indices() {
                    control.check()?;
                    let alphanumeric = characters.is_alphanumeric(character, !full);
                    let kind = if previous == Some(alphanumeric) {
                        CaseKind::Lower
                    } else if full {
                        CaseKind::Title
                    } else {
                        CaseKind::Upper
                    };
                    previous = Some(alphanumeric);
                    if full {
                        characters.push_full(
                            text,
                            offset,
                            character,
                            kind,
                            &mut output,
                            control,
                        )?;
                    } else {
                        output.push(characters.simple(character, kind))?;
                    }
                }
            }
            BuiltinCollationCtype::IcuRoot => icu_titlecase(text, &mut output, control)?,
        }
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
        let characters = UnicodeCharacters::new(if self.ctype == BuiltinCollationCtype::IcuRoot {
            FinalSigmaRule::Icu
        } else {
            FinalSigmaRule::Builtin
        });
        for (offset, character) in text.char_indices() {
            control.check()?;
            match self.ctype {
                BuiltinCollationCtype::Ascii => output.push(match kind {
                    CaseKind::Upper => character.to_ascii_uppercase(),
                    _ => character.to_ascii_lowercase(),
                })?,
                BuiltinCollationCtype::UnicodeSimple => {
                    output.push(characters.simple(character, kind))?;
                }
                BuiltinCollationCtype::UnicodeFull | BuiltinCollationCtype::IcuRoot => {
                    characters.push_full(text, offset, character, kind, &mut output, control)?;
                }
            }
        }
        output.finish()
    }
}

/// ICU's `ustrcase_internalToTitle` with its default break adjustment: in each word, characters before the first letter, number, symbol, private-use character or cased modifier letter are copied, that character maps to titlecase, and the rest of the word maps to lowercase in the context of the whole string.
fn icu_titlecase(
    text: &str,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    let characters = UnicodeCharacters::new(FinalSigmaRule::Icu);
    let boundaries = word_breaks::boundaries(text, control)?;
    for segment in boundaries.windows(2) {
        let (start, end) = (segment[0], segment[1]);
        let mut titled = false;
        for (offset, character) in text[start..end].char_indices() {
            control.check()?;
            if titled {
                characters.push_full(
                    text,
                    start + offset,
                    character,
                    CaseKind::Lower,
                    output,
                    control,
                )?;
            } else if characters.is_title_start(character) {
                characters.push_full(
                    text,
                    start + offset,
                    character,
                    CaseKind::Title,
                    output,
                    control,
                )?;
                titled = true;
            } else {
                output.push(character)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
