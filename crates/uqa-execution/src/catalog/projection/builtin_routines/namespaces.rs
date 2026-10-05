//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 temporary-namespace inquiry routine metadata.

use super::BuiltinRoutineCatalogEntry;

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    BuiltinRoutineCatalogEntry {
        oid: 2854,
        name: "pg_my_temp_schema",
        kind: "f",
        strict: true,
        volatility: "s",
        parallel: "r",
        leakproof: false,
        return_type: 26,
        argument_types: &[],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "pg_my_temp_schema",
    },
    BuiltinRoutineCatalogEntry {
        oid: 2855,
        name: "pg_is_other_temp_schema",
        kind: "f",
        strict: true,
        volatility: "s",
        parallel: "s",
        leakproof: false,
        return_type: 16,
        argument_types: &[26],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "pg_is_other_temp_schema",
    },
];
