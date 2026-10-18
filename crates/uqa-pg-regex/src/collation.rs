//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collation selection with `PostgreSQL`'s `pg_set_regex_collation` rules.

use crate::error::RegexError;

/// The ctype behavior that a collation selects for character classes, case-insensitive matching and word boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegexCollation {
    /// `PostgreSQL`'s C strategy, for `C`, `POSIX` and the builtin provider's `C` locale (`ucs_basic`): hard-wired ASCII classes and ASCII-only case mapping.
    C,
    /// The builtin provider's `C.UTF-8` locale (`pg_c_utf8`): Unicode classes with the POSIX-compatible variants of `digit`, `alnum` and `punct`, and simple case mapping.
    BuiltinCUtf8,
    /// The builtin provider's `PG_UNICODE_FAST` locale (`pg_unicode_fast` and UQA's database default): Unicode classes with the standard variants, and simple case mapping.
    BuiltinUnicodeFast,
    /// The ICU provider (`unicode` and other ICU collations): ICU's character functions, which do not depend on the locale.
    Icu,
}

/// How a collation defines its ctype, as `pg_set_regex_collation` distinguishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CollationCtype {
    /// The ctype is `C` or `POSIX` (`ctype_is_c`), for any provider.
    C,
    /// The builtin provider with another locale; `casemap_full` is true for `PG_UNICODE_FAST` and false for `C.UTF-8`.
    Builtin { casemap_full: bool },
    /// The ICU provider.
    Icu,
}

/// The catalog properties of a collation that regular expressions consult.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CollationLocale {
    pub deterministic: bool,
    pub ctype: CollationCtype,
}

/// `C_COLLATION_OID`.
const C_COLLATION_OID: u32 = 950;
const DEFAULT_COLLATION_OID: u32 = 100;

/// UQA's predefined collations. All are deterministic; the database default uses the builtin `PG_UNICODE_FAST` locale.
const PREDEFINED: [(u32, CollationCtype); 7] = [
    (
        DEFAULT_COLLATION_OID,
        CollationCtype::Builtin { casemap_full: true },
    ),
    (
        811,
        CollationCtype::Builtin {
            casemap_full: false,
        },
    ),
    (C_COLLATION_OID, CollationCtype::C),
    (951, CollationCtype::C),
    (962, CollationCtype::C),
    (963, CollationCtype::Icu),
    (6411, CollationCtype::Builtin { casemap_full: true }),
];

impl RegexCollation {
    /// Resolve one of UQA's predefined collations by OID: `default` (100), `pg_c_utf8` (811), `C` (950), `POSIX` (951), `ucs_basic` (962), `unicode` (963) and `pg_unicode_fast` (6411).
    ///
    /// OID 0 fails as `PostgreSQL` fails when the parser could not resolve a collation (`42P22`, with its hint), and any other OID fails as `PostgreSQL`'s catalog lookup of a missing collation does (`XX000`).
    pub fn from_oid(oid: u32) -> Result<Self, RegexError> {
        if oid == 0 || oid == C_COLLATION_OID {
            return Self::from_locale(
                oid,
                CollationLocale {
                    deterministic: true,
                    ctype: CollationCtype::C,
                },
            );
        }
        let Some((_, ctype)) = PREDEFINED.iter().find(|(candidate, _)| *candidate == oid) else {
            return Err(RegexError::UnknownCollation(oid));
        };
        Self::from_locale(
            oid,
            CollationLocale {
                deterministic: true,
                ctype: *ctype,
            },
        )
    }

    /// Apply `pg_set_regex_collation` to a collation with the given catalog properties. The order of the checks is `PostgreSQL`'s: an unresolved collation (OID 0), then `C`, then nondeterminism (`0A000`), then the ctype.
    pub fn from_locale(oid: u32, locale: CollationLocale) -> Result<Self, RegexError> {
        if oid == 0 {
            return Err(RegexError::IndeterminateCollation);
        }
        if oid == C_COLLATION_OID {
            return Ok(Self::C);
        }
        if !locale.deterministic {
            return Err(RegexError::NondeterministicCollation);
        }
        Ok(match locale.ctype {
            CollationCtype::C => Self::C,
            CollationCtype::Builtin {
                casemap_full: false,
            } => Self::BuiltinCUtf8,
            CollationCtype::Builtin { casemap_full: true } => Self::BuiltinUnicodeFast,
            CollationCtype::Icu => Self::Icu,
        })
    }

    /// The locale token of `uqa_pg_regex.h` that the C shim receives as the engine's collation argument.
    pub(crate) const fn token(self) -> u32 {
        match self {
            Self::C => 1,
            Self::BuiltinCUtf8 => 2,
            Self::BuiltinUnicodeFast => 3,
            Self::Icu => 4,
        }
    }
}
