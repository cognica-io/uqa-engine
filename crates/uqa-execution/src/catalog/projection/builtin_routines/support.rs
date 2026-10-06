//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identities of the planner support functions admitted by routine declarations.

use super::BuiltinRoutineCatalogEntry;

const fn routine(oid: i64, name: &'static str) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "f",
        strict: true,
        volatility: "i",
        parallel: "s",
        leakproof: false,
        return_type: 2281,
        argument_types: &[2281],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: name,
    }
}

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    routine(1023, "textlike_support"),
    routine(1024, "texticregexeq_support"),
    routine(1025, "texticlike_support"),
    routine(1173, "network_subset_support"),
    routine(1364, "textregexeq_support"),
    routine(3097, "varchar_support"),
    routine(3157, "numeric_support"),
    routine(6380, "array_subscript_handler_support"),
];
