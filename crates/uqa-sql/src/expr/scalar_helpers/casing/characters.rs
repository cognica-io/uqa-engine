//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unicode character properties and per-character case mappings shared by the builtin and ICU case-mapping providers.

use super::output::MappedOutput;
use icu_casemap::{
    options::{LeadingAdjustment, TitlecaseOptions, TrailingCase},
    CaseMapper, CaseMapperBorrowed,
};
use icu_locale_core::LanguageIdentifier;
use icu_properties::{
    props::{Alphabetic, CaseIgnorable, Cased, GeneralCategory, GeneralCategoryGroup},
    CodePointMapData, CodePointMapDataBorrowed, CodePointSetData, CodePointSetDataBorrowed,
};
use uqa_core::{memory::ProductionControl, ValueRetentionError};

const CAPITAL_SIGMA: char = '\u{03A3}';
const SMALL_SIGMA: char = '\u{03C3}';
const FINAL_SIGMA: char = '\u{03C2}';

/// The case kind one character is mapped to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaseKind {
    Lower,
    Upper,
    Title,
    Fold,
}

/// Which context rule decides `Final_Sigma` for a capital sigma in a full lowercase mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FinalSigmaRule {
    /// `PostgreSQL`'s builtin `check_final_sigma`: a sigma that only case-ignorable characters precede still passes the preceding test.
    Builtin,
    /// ICU's `isFollowedByCasedLetter` in both directions: a cased letter must precede the sigma.
    Icu,
}

/// The pinned Unicode character data used by every non-ASCII case-mapping provider. ICU4X 2.0 and `PostgreSQL` 18.4 both carry Unicode 16.0 tables.
#[derive(Clone, Copy)]
pub(super) struct UnicodeCharacters {
    sigma: FinalSigmaRule,
    mapper: CaseMapperBorrowed<'static>,
    alphabetic: CodePointSetDataBorrowed<'static>,
    cased: CodePointSetDataBorrowed<'static>,
    case_ignorable: CodePointSetDataBorrowed<'static>,
    general_category: CodePointMapDataBorrowed<'static, GeneralCategory>,
}

impl UnicodeCharacters {
    pub(super) const fn new(sigma: FinalSigmaRule) -> Self {
        Self {
            sigma,
            mapper: CaseMapper::new(),
            alphabetic: CodePointSetData::new::<Alphabetic>(),
            cased: CodePointSetData::new::<Cased>(),
            case_ignorable: CodePointSetData::new::<CaseIgnorable>(),
            general_category: CodePointMapData::<GeneralCategory>::new(),
        }
    }

    pub(super) fn general_category(self, character: char) -> GeneralCategory {
        self.general_category.get(character)
    }

    /// `pg_u_isalnum`: Alphabetic, or a decimal digit (ASCII only in the POSIX-compatible form).
    pub(super) fn is_alphanumeric(self, character: char, posix: bool) -> bool {
        self.alphabetic.contains(character)
            || if posix {
                character.is_ascii_digit()
            } else {
                self.general_category(character) == GeneralCategory::DecimalNumber
            }
    }

    /// ICU's `ustrcase_isLNS`: a letter, number, symbol or private-use character, where a modifier letter counts only when it is cased.
    pub(super) fn is_title_start(self, character: char) -> bool {
        let category = self.general_category(character);
        if category == GeneralCategory::ModifierLetter {
            return self.cased.contains(character);
        }
        GeneralCategoryGroup::Letter
            .union(GeneralCategoryGroup::Number)
            .union(GeneralCategoryGroup::Symbol)
            .union(GeneralCategoryGroup::PrivateUse)
            .contains(category)
    }

    /// The simple (single-character) mapping, as `PostgreSQL`'s `pg_c_utf8` and ICU's simple functions read `UnicodeData.txt` and `CaseFolding.txt` statuses C and S.
    pub(super) fn simple(self, character: char, kind: CaseKind) -> char {
        match kind {
            CaseKind::Lower => self.mapper.simple_lowercase(character),
            CaseKind::Upper => self.mapper.simple_uppercase(character),
            CaseKind::Title => self.mapper.simple_titlecase(character),
            CaseKind::Fold => self.mapper.simple_fold(character),
        }
    }

    /// Write the full mapping of the character at `offset` in `text`. Only a capital sigma depends on its context in the root locale; every other full mapping is the unconditional `SpecialCasing.txt` entry or the simple mapping.
    pub(super) fn push_full(
        self,
        text: &str,
        offset: usize,
        character: char,
        kind: CaseKind,
        output: &mut MappedOutput<'_, '_>,
        control: &ProductionControl<'_>,
    ) -> Result<(), ValueRetentionError> {
        if character == CAPITAL_SIGMA && kind == CaseKind::Lower {
            return output.push(self.lowercase_sigma(text, offset, control)?);
        }
        let source = &text[offset..offset + character.len_utf8()];
        let root = LanguageIdentifier::UNKNOWN;
        match kind {
            CaseKind::Lower => output.write(&self.mapper.lowercase(source, &root)),
            CaseKind::Upper => output.write(&self.mapper.uppercase(source, &root)),
            CaseKind::Fold => output.write(&self.mapper.fold(source)),
            CaseKind::Title => {
                let mut options = TitlecaseOptions::default();
                options.leading_adjustment = Some(LeadingAdjustment::None);
                options.trailing_case = Some(TrailingCase::Unchanged);
                output.write(
                    &self
                        .mapper
                        .titlecase_segment_with_only_case_data(source, &root, options),
                )
            }
        }
    }

    /// Write the full lowercase, uppercase or case folding of `text[start..end]`. These mappings are context-free in the root locale except for a capital sigma's lowercase, so the run maps as whole strings between capital sigmas and each sigma reads the whole text.
    pub(super) fn push_full_run(
        self,
        text: &str,
        start: usize,
        end: usize,
        kind: CaseKind,
        output: &mut MappedOutput<'_, '_>,
        control: &ProductionControl<'_>,
    ) -> Result<(), ValueRetentionError> {
        let root = LanguageIdentifier::UNKNOWN;
        let run = &text[start..end];
        match kind {
            CaseKind::Upper => return output.write(&self.mapper.uppercase(run, &root)),
            CaseKind::Fold => return output.write(&self.mapper.fold(run)),
            CaseKind::Title => unreachable!("titlecase maps one word start at a time"),
            CaseKind::Lower => {}
        }
        let mut segment = start;
        for (offset, character) in run.char_indices() {
            if character != CAPITAL_SIGMA {
                continue;
            }
            let sigma = start + offset;
            if sigma > segment {
                output.write(&self.mapper.lowercase(&text[segment..sigma], &root))?;
            }
            output.push(self.lowercase_sigma(text, sigma, control)?)?;
            segment = sigma + CAPITAL_SIGMA.len_utf8();
        }
        if segment < end {
            output.write(&self.mapper.lowercase(&text[segment..end], &root))?;
        }
        Ok(())
    }

    /// The lowercase of the capital sigma at `offset`, as this provider decides `Final_Sigma`.
    fn lowercase_sigma(
        self,
        text: &str,
        offset: usize,
        control: &ProductionControl<'_>,
    ) -> Result<char, ValueRetentionError> {
        let after = offset + CAPITAL_SIGMA.len_utf8();
        let final_form = match self.sigma {
            FinalSigmaRule::Builtin => self.builtin_final_sigma(text, offset, after, control)?,
            FinalSigmaRule::Icu => self.icu_final_sigma(text, offset, after, control)?,
        };
        Ok(if final_form { FINAL_SIGMA } else { SMALL_SIGMA })
    }

    /// `check_final_sigma` in `src/common/unicode_case.c`. Its backward scan returns false only on an uncased, non-ignorable character, so a run of ignorable characters back to the start still passes.
    fn builtin_final_sigma(
        self,
        text: &str,
        offset: usize,
        after: usize,
        control: &ProductionControl<'_>,
    ) -> Result<bool, ValueRetentionError> {
        if offset == 0 {
            return Ok(false);
        }
        for (scanned, character) in text[..offset].chars().rev().enumerate() {
            checkpoint(scanned, control)?;
            if self.case_ignorable.contains(character) {
                continue;
            }
            if self.cased.contains(character) {
                break;
            }
            return Ok(false);
        }
        self.not_followed_by_cased(&text[after..], control)
    }

    /// ICU `ucase_toFullLower`: not followed by a cased letter, and preceded by one, skipping case-ignorable characters in both directions.
    fn icu_final_sigma(
        self,
        text: &str,
        offset: usize,
        after: usize,
        control: &ProductionControl<'_>,
    ) -> Result<bool, ValueRetentionError> {
        if !self.not_followed_by_cased(&text[after..], control)? {
            return Ok(false);
        }
        for (scanned, character) in text[..offset].chars().rev().enumerate() {
            checkpoint(scanned, control)?;
            if self.case_ignorable.contains(character) {
                continue;
            }
            return Ok(self.cased.contains(character));
        }
        Ok(false)
    }

    fn not_followed_by_cased(
        self,
        following: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, ValueRetentionError> {
        for (scanned, character) in following.chars().enumerate() {
            checkpoint(scanned, control)?;
            if self.case_ignorable.contains(character) {
                continue;
            }
            return Ok(!self.cased.contains(character));
        }
        Ok(true)
    }
}

/// Observe both cancellation scopes once per bounded run of a context scan.
fn checkpoint(scanned: usize, control: &ProductionControl<'_>) -> Result<(), ValueRetentionError> {
    if scanned.is_multiple_of(1024) {
        control.check()?;
    }
    Ok(())
}
