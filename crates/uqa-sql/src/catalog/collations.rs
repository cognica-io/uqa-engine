//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in collation identities shared by declarations and OID alias input/output.

/// Predefined `PostgreSQL` 18 collation names and OIDs.
const BUILTIN_COLLATIONS: [(&str, i64); 7] = [
    ("default", 100),
    ("pg_c_utf8", 811),
    ("C", 950),
    ("POSIX", 951),
    ("ucs_basic", 962),
    ("unicode", 963),
    ("pg_unicode_fast", 6411),
];

/// The `pg_collation` OID of a built-in collation name.
#[must_use]
pub fn builtin_collation_oid(name: &str) -> Option<i64> {
    BUILTIN_COLLATIONS
        .iter()
        .find(|(collation, _)| *collation == name)
        .map(|(_, oid)| *oid)
}

/// The name of a built-in collation selected by its catalog OID.
#[must_use]
pub fn builtin_collation_name(oid: i64) -> Option<&'static str> {
    BUILTIN_COLLATIONS
        .iter()
        .find_map(|(name, candidate)| (*candidate == oid).then_some(*name))
}
