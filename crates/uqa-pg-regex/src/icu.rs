//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Check the category and simple case tables used by the ICU strategy against ICU4X's Unicode 16 data.
#![allow(unsafe_code)] // The shim links PostgreSQL's Unicode table lookups under these names.

use icu_casemap::CaseMapper;
use icu_properties::props::GeneralCategory;
use icu_properties::CodePointMapData;
use std::ffi::c_int;

extern "C" {
    fn uqa_pg_regex_unicode_category(code: u32) -> c_int;
    fn uqa_pg_regex_unicode_uppercase_simple(code: u32) -> u32;
    fn uqa_pg_regex_unicode_lowercase_simple(code: u32) -> u32;
}

#[test]
fn general_categories_match_unicode_16() {
    let categories = CodePointMapData::<GeneralCategory>::new();
    for code in 0..=0x10_FFFF {
        // SAFETY: PostgreSQL accepts every Unicode code point, including surrogates.
        let actual = unsafe { uqa_pg_regex_unicode_category(code) };
        assert_eq!(actual, categories.get32(code) as c_int, "U+{code:04X}");
    }
}

#[test]
fn simple_case_mappings_match_unicode_16() {
    let mapper = CaseMapper::new();
    for code in (0..=0x10_FFFF).chain([0x11_0000, 0x7FFF_FFFE, u32::MAX]) {
        let (upper, lower) = char::from_u32(code).map_or((code, code), |c| {
            (
                u32::from(mapper.simple_uppercase(c)),
                u32::from(mapper.simple_lowercase(c)),
            )
        });
        // SAFETY: both lookups return the input when no mapping exists.
        unsafe {
            assert_eq!(
                uqa_pg_regex_unicode_uppercase_simple(code),
                upper,
                "U+{code:04X}"
            );
            assert_eq!(
                uqa_pg_regex_unicode_lowercase_simple(code),
                lower,
                "U+{code:04X}"
            );
        }
    }
}
