//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

/// Character classification and case mapping of a fixed built-in collation, as `PostgreSQL` derives them from the collation's provider and locale (`pg_locale_t`). The database default is UQA's builtin `PG_UNICODE_FAST`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinCollationCtype {
    /// `ctype_is_c`: libc `C` and `POSIX` and builtin `C` (`ucs_basic`) classify and map only ASCII letters.
    Ascii,
    /// Builtin `C.UTF-8` (`pg_c_utf8`): Unicode properties with POSIX-compatible digits and simple case mappings.
    UnicodeSimple,
    /// Builtin `PG_UNICODE_FAST` (`pg_unicode_fast` and the database default): Unicode properties with full case mappings, including conditional special casing.
    UnicodeFull,
    /// ICU root locale `und` (`unicode`): ICU case mappings and word boundaries.
    IcuRoot,
}
