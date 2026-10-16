//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`'s undefined-type I/O identities, independent of the type that will complete a shell.

use super::BuiltinRoutineCatalogEntry;

const fn function(
    oid: i64,
    name: &'static str,
    strict: bool,
    return_type: i64,
    argument_types: &'static [i64],
) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "f",
        strict,
        volatility: "i",
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
    function(2398, "shell_in", false, 2278, &[2275]),
    function(2399, "shell_out", true, 2275, &[2278]),
];
