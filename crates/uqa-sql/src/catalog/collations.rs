//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in collation identities shared by declarations and OID alias input/output.

mod ctype;
pub mod declarations;
pub(crate) mod derivation;
pub mod expression;
mod selection;

pub use ctype::BuiltinCollationCtype;
pub use selection::CollationSelection;

/// The database-default collation's predefined `PostgreSQL` catalog identity.
pub const DEFAULT_COLLATION_OID: i64 = 100;
pub const C_COLLATION_OID: u32 = 950;

/// Predefined `PostgreSQL` 18 collation names and OIDs with their ordering and character semantics.
const BUILTIN_COLLATIONS: [(&str, i64, BuiltinCollationOrder, BuiltinCollationCtype); 7] = [
    (
        "default",
        DEFAULT_COLLATION_OID,
        BuiltinCollationOrder::Bytes,
        BuiltinCollationCtype::UnicodeFull,
    ),
    (
        "pg_c_utf8",
        811,
        BuiltinCollationOrder::Bytes,
        BuiltinCollationCtype::UnicodeSimple,
    ),
    (
        "C",
        C_COLLATION_OID as i64,
        BuiltinCollationOrder::Bytes,
        BuiltinCollationCtype::Ascii,
    ),
    (
        "POSIX",
        951,
        BuiltinCollationOrder::Bytes,
        BuiltinCollationCtype::Ascii,
    ),
    (
        "ucs_basic",
        962,
        BuiltinCollationOrder::Bytes,
        BuiltinCollationCtype::Ascii,
    ),
    (
        "unicode",
        963,
        BuiltinCollationOrder::Unicode,
        BuiltinCollationCtype::IcuRoot,
    ),
    (
        "pg_unicode_fast",
        6411,
        BuiltinCollationOrder::Bytes,
        BuiltinCollationCtype::UnicodeFull,
    ),
];

/// The `pg_collation` OID of a built-in collation name.
#[must_use]
pub fn builtin_collation_oid(name: &str) -> Option<i64> {
    BUILTIN_COLLATIONS
        .iter()
        .find(|(collation, ..)| *collation == name)
        .map(|(_, oid, ..)| *oid)
}

/// The name of a built-in collation selected by its catalog OID.
#[must_use]
pub fn builtin_collation_name(oid: i64) -> Option<&'static str> {
    BUILTIN_COLLATIONS
        .iter()
        .find_map(|(name, candidate, ..)| (*candidate == oid).then_some(*name))
}

/// Executable ordering of the fixed deterministic built-ins. The database default is UQA's builtin `PG_UNICODE_FAST`; selecting `unicode` chooses root ICU instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinCollationOrder {
    Bytes,
    Unicode,
}

pub fn builtin_collation_order(oid: u32) -> Option<BuiltinCollationOrder> {
    BUILTIN_COLLATIONS
        .iter()
        .find_map(|(_, candidate, order, _)| (*candidate == i64::from(oid)).then_some(*order))
}

/// The character semantics of a built-in collation selected by its catalog OID.
pub fn builtin_collation_ctype(oid: u32) -> Option<BuiltinCollationCtype> {
    BUILTIN_COLLATIONS
        .iter()
        .find_map(|(_, candidate, _, ctype)| (*candidate == i64::from(oid)).then_some(*ctype))
}
