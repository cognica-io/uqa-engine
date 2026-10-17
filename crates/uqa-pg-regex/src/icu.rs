//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ICU4C's character functions for `PostgreSQL`'s ICU regular expression strategy, evaluated over ICU4X data.
//!
//! `PostgreSQL`'s ICU strategy calls `u_isdigit`, `u_isalpha`, `u_isalnum`, `u_isupper`, `u_islower`, `u_isgraph`, `u_isprint`, `u_ispunct`, `u_isspace`, `u_toupper` and `u_tolower`. None of them depends on the collation's locale. ICU4C defines each classification by the code point's `General_Category`, as implemented in `uchar.cpp`:
//!
//! - `u_isdigit`: Nd. `u_isalpha`: L (Lu, Ll, Lt, Lm, Lo). `u_isalnum`: L or Nd.
//! - `u_isupper`: Lu. `u_islower`: Ll. `u_ispunct`: P (Pc, Pd, Ps, Pe, Pi, Pf, Po).
//! - `u_isgraph`: none of Cc, Cf, Cs, Cn, Zs, Zl, Zp. `u_isprint`: none of Cc, Cf, Cs, Co, Cn.
//! - `u_isspace`: Z (Zs, Zl, Zp), or a control space: U+0009..U+000D, U+001C..U+001F or U+0085.
//!
//! `u_toupper` and `u_tolower` return the Unicode Character Database's simple case mapping, or the code point itself when it has none. ICU4X's [`CodePointMapData`] for [`GeneralCategory`] and [`CaseMapper`]'s simple mappings expose the same Unicode Character Database properties, so these functions equal ICU4C's whenever both use the same Unicode version; the workspace pins ICU4X data for Unicode 16, the version of `PostgreSQL` 18.4's own tables. ICU4X numbers `General_Category` values as ICU4C's `UCharCategory` does. Code points above U+10FFFF have no properties and map to themselves, as in ICU4C.
#![allow(unsafe_code)] // These functions are exported to the C shim by name.

use icu_casemap::CaseMapper;
use icu_properties::props::GeneralCategory;
use icu_properties::CodePointMapData;
use std::ffi::c_int;

const fn mask(category: GeneralCategory) -> u32 {
    1 << category as u32
}

const LETTER: u32 = mask(GeneralCategory::UppercaseLetter)
    | mask(GeneralCategory::LowercaseLetter)
    | mask(GeneralCategory::TitlecaseLetter)
    | mask(GeneralCategory::ModifierLetter)
    | mask(GeneralCategory::OtherLetter);
const DECIMAL_NUMBER: u32 = mask(GeneralCategory::DecimalNumber);
const PUNCTUATION: u32 = mask(GeneralCategory::ConnectorPunctuation)
    | mask(GeneralCategory::DashPunctuation)
    | mask(GeneralCategory::OpenPunctuation)
    | mask(GeneralCategory::ClosePunctuation)
    | mask(GeneralCategory::InitialPunctuation)
    | mask(GeneralCategory::FinalPunctuation)
    | mask(GeneralCategory::OtherPunctuation);
const SEPARATOR: u32 = mask(GeneralCategory::SpaceSeparator)
    | mask(GeneralCategory::LineSeparator)
    | mask(GeneralCategory::ParagraphSeparator);
const OTHER: u32 = mask(GeneralCategory::Control)
    | mask(GeneralCategory::Format)
    | mask(GeneralCategory::Surrogate)
    | mask(GeneralCategory::PrivateUse)
    | mask(GeneralCategory::Unassigned);
const NOT_GRAPHIC: u32 = mask(GeneralCategory::Control)
    | mask(GeneralCategory::Format)
    | mask(GeneralCategory::Surrogate)
    | mask(GeneralCategory::Unassigned)
    | SEPARATOR;

/// The highest Unicode code point.
const MAX_CODE_POINT: u32 = 0x10_FFFF;

pub(crate) fn general_category(code: u32) -> GeneralCategory {
    if code > MAX_CODE_POINT {
        return GeneralCategory::Unassigned;
    }
    CodePointMapData::<GeneralCategory>::new().get32(code)
}

fn category_in(code: u32, categories: u32) -> c_int {
    c_int::from(mask(general_category(code)) & categories != 0)
}

fn category_is(code: u32, category: GeneralCategory) -> c_int {
    c_int::from(general_category(code) == category)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_isdigit(code: u32) -> c_int {
    category_is(code, GeneralCategory::DecimalNumber)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_isalpha(code: u32) -> c_int {
    category_in(code, LETTER)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_isalnum(code: u32) -> c_int {
    category_in(code, LETTER | DECIMAL_NUMBER)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_isupper(code: u32) -> c_int {
    category_is(code, GeneralCategory::UppercaseLetter)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_islower(code: u32) -> c_int {
    category_is(code, GeneralCategory::LowercaseLetter)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_isgraph(code: u32) -> c_int {
    c_int::from(mask(general_category(code)) & NOT_GRAPHIC == 0)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_isprint(code: u32) -> c_int {
    c_int::from(mask(general_category(code)) & OTHER == 0)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_ispunct(code: u32) -> c_int {
    category_in(code, PUNCTUATION)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_isspace(code: u32) -> c_int {
    let control_space = matches!(code, 0x09..=0x0D | 0x1C..=0x1F | 0x85);
    c_int::from(control_space || category_in(code, SEPARATOR) != 0)
}

fn simple_mapping(code: u32, map: impl FnOnce(char) -> char) -> u32 {
    char::from_u32(code).map_or(code, |character| u32::from(map(character)))
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_toupper(code: u32) -> u32 {
    simple_mapping(code, |character| {
        CaseMapper::new().simple_uppercase(character)
    })
}

#[no_mangle]
extern "C" fn uqa_pg_regex_icu_tolower(code: u32) -> u32 {
    simple_mapping(code, |character| {
        CaseMapper::new().simple_lowercase(character)
    })
}

#[cfg(test)]
mod tests;
