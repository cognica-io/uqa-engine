//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` identities of the set-returning `regexp_matches` overloads.

use super::BuiltinRoutineCatalogEntry;

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    BuiltinRoutineCatalogEntry {
        oid: 2763,
        name: "regexp_matches",
        kind: "f",
        strict: true,
        volatility: "i",
        parallel: "s",
        leakproof: false,
        return_type: 1009,
        argument_types: &[25, 25],
        argument_names: &["string", "pattern"],
        default_arguments: 0,
        argument_defaults: None,
        source: "regexp_matches_no_flags",
    },
    BuiltinRoutineCatalogEntry {
        oid: 2764,
        name: "regexp_matches",
        kind: "f",
        strict: true,
        volatility: "i",
        parallel: "s",
        leakproof: false,
        return_type: 1009,
        argument_types: &[25, 25, 25],
        argument_names: &["string", "pattern", "flags"],
        default_arguments: 0,
        argument_defaults: None,
        source: "regexp_matches",
    },
];
