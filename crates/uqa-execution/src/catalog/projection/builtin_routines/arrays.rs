//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 array input, output, binary and statistics routines that every array `pg_type` row references.

use super::BuiltinRoutineCatalogEntry;

const fn routine(
    oid: i64,
    name: &'static str,
    return_type: i64,
    argument_types: &'static [i64],
) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "f",
        strict: true,
        volatility: "s",
        parallel: "s",
        leakproof: false,
        return_type,
        argument_types,
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: name,
    }
}

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    routine(750, "array_in", 2277, &[2275, 26, 23]),
    routine(751, "array_out", 2275, &[2277]),
    routine(2400, "array_recv", 2277, &[2281, 26, 23]),
    routine(2401, "array_send", 17, &[2277]),
    routine(3816, "array_typanalyze", 16, &[2281]),
];
