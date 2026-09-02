//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::BuiltinRoutineCatalogEntry;

pub(super) const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    BuiltinRoutineCatalogEntry {
        oid: 3078,
        name: "pg_sequence_parameters",
        kind: "f",
        strict: true,
        volatility: "s",
        parallel: "s",
        leakproof: false,
        return_type: 2249,
        argument_types: &[26],
        argument_names: &[
            "sequence_oid",
            "start_value",
            "minimum_value",
            "maximum_value",
            "increment",
            "cycle_option",
            "cache_size",
            "data_type",
        ],
        default_arguments: 0,
        argument_defaults: None,
        source: "pg_sequence_parameters",
    },
    BuiltinRoutineCatalogEntry {
        oid: 4032,
        name: "pg_sequence_last_value",
        kind: "f",
        strict: true,
        volatility: "v",
        parallel: "u",
        leakproof: false,
        return_type: 20,
        argument_types: &[2205],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "pg_sequence_last_value",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6427,
        name: "pg_get_sequence_data",
        kind: "f",
        strict: true,
        volatility: "v",
        parallel: "u",
        leakproof: false,
        return_type: 2249,
        argument_types: &[2205],
        argument_names: &["sequence_oid", "last_value", "is_called"],
        default_arguments: 0,
        argument_defaults: None,
        source: "pg_get_sequence_data",
    },
];
