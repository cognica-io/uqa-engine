//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compiler-independent character properties for the persisted analyzer profile.

use icu_properties::{
    props::{CaseIgnorable, Cased, WhiteSpace},
    CodePointSetData, CodePointSetDataBorrowed,
};

// The pinned ICU4X 2.0 data preserves the Unicode 16 behavior of Rust 1.90.
pub(crate) const VERSION: (u8, u8, u8) = (16, 0, 0);
pub(crate) const CASED: CodePointSetDataBorrowed<'static> = CodePointSetData::new::<Cased>();
pub(crate) const CASE_IGNORABLE: CodePointSetDataBorrowed<'static> =
    CodePointSetData::new::<CaseIgnorable>();

pub(crate) fn is_whitespace(character: char) -> bool {
    CodePointSetData::new::<WhiteSpace>().contains(character)
}
