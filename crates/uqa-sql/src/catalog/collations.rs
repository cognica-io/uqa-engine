//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in collation identities shared by declarations and OID alias input/output.

pub mod declarations;
pub(crate) mod derivation;
pub mod expression;

/// The database-default collation's predefined `PostgreSQL` catalog identity.
pub const DEFAULT_COLLATION_OID: i64 = 100;
pub const C_COLLATION_OID: u32 = 950;

/// Predefined `PostgreSQL` 18 collation names and OIDs.
const BUILTIN_COLLATIONS: [(&str, i64, BuiltinCollationOrder); 7] = [
    (
        "default",
        DEFAULT_COLLATION_OID,
        BuiltinCollationOrder::Bytes,
    ),
    ("pg_c_utf8", 811, BuiltinCollationOrder::Bytes),
    ("C", C_COLLATION_OID as i64, BuiltinCollationOrder::Bytes),
    ("POSIX", 951, BuiltinCollationOrder::Bytes),
    ("ucs_basic", 962, BuiltinCollationOrder::Bytes),
    ("unicode", 963, BuiltinCollationOrder::Unicode),
    ("pg_unicode_fast", 6411, BuiltinCollationOrder::Bytes),
];

/// The `pg_collation` OID of a built-in collation name.
#[must_use]
pub fn builtin_collation_oid(name: &str) -> Option<i64> {
    BUILTIN_COLLATIONS
        .iter()
        .find(|(collation, _, _)| *collation == name)
        .map(|(_, oid, _)| *oid)
}

/// The name of a built-in collation selected by its catalog OID.
#[must_use]
pub fn builtin_collation_name(oid: i64) -> Option<&'static str> {
    BUILTIN_COLLATIONS
        .iter()
        .find_map(|(name, candidate, _)| (*candidate == oid).then_some(*name))
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
        .find_map(|(_, candidate, order)| (*candidate == i64::from(oid)).then_some(*order))
}
