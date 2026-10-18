//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ICU4X data against `PostgreSQL` 18.4's own Unicode 16 tables, compiled from the imported `unicode_category.c` and `unicode_case.c`.

use super::*;

extern "C" {
    /// `unicode_category()`, returning a `pg_unicode_category`, whose values are ICU's `UCharCategory` numbers.
    fn uqa_pg_regex_unicode_category(code: u32) -> c_int;
    fn uqa_pg_regex_unicode_uppercase_simple(code: u32) -> u32;
    fn uqa_pg_regex_unicode_lowercase_simple(code: u32) -> u32;
}

/// Every code point, plus values above U+10FFFF that pattern escapes can produce.
fn code_points() -> impl Iterator<Item = u32> {
    (0..=MAX_CODE_POINT).chain([0x11_0000, 0x7FFF_FFFE])
}

#[test]
fn general_categories_equal_postgresql_tables() {
    for code in code_points() {
        // SAFETY: the table lookup accepts every value.
        let postgresql = unsafe { uqa_pg_regex_unicode_category(code) };
        assert_eq!(general_category(code) as c_int, postgresql, "U+{code:04X}");
    }
}

#[test]
fn simple_case_mappings_equal_postgresql_tables() {
    for code in code_points() {
        // SAFETY: the table lookups accept every value.
        let (upper, lower) = unsafe {
            (
                uqa_pg_regex_unicode_uppercase_simple(code),
                uqa_pg_regex_unicode_lowercase_simple(code),
            )
        };
        assert_eq!(uqa_pg_regex_icu_toupper(code), upper, "U+{code:04X}");
        assert_eq!(uqa_pg_regex_icu_tolower(code), lower, "U+{code:04X}");
    }
}

#[test]
fn classifications_follow_icu4c_definitions() {
    // U+00AD SOFT HYPHEN (Cf) is neither graphic nor printable; U+E000 (Co) is graphic but not printable.
    assert_eq!(uqa_pg_regex_icu_isgraph(0xAD), 0);
    assert_eq!(uqa_pg_regex_icu_isprint(0xAD), 0);
    assert_eq!(uqa_pg_regex_icu_isgraph(0xE000), 1);
    assert_eq!(uqa_pg_regex_icu_isprint(0xE000), 0);
    // U+216B ROMAN NUMERAL TWELVE (Nl) is alphabetic in Unicode but not a letter for ICU.
    assert_eq!(uqa_pg_regex_icu_isalpha(0x216B), 0);
    // U+0085 is a control space; U+00A0 is a space separator.
    assert_eq!(uqa_pg_regex_icu_isspace(0x85), 1);
    assert_eq!(uqa_pg_regex_icu_isspace(0xA0), 1);
    assert_eq!(uqa_pg_regex_icu_ispunct(u32::from('$')), 0);
    assert_eq!(uqa_pg_regex_icu_isdigit(0x0663), 1);
    assert_eq!(uqa_pg_regex_icu_toupper(0x0011_0000), 0x0011_0000);
}
