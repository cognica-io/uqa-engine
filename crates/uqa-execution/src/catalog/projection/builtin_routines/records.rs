//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 composite type I/O routines, which every composite type names in `pg_type`.

use super::BuiltinRoutineCatalogEntry;

const fn function(
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
    function(2290, "record_in", 2249, &[2275, 26, 23]),
    function(2291, "record_out", 2275, &[2249]),
    function(2402, "record_recv", 2249, &[2281, 26, 23]),
    function(2403, "record_send", 17, &[2249]),
];
