//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Implemented integer series and their planner support identities.

use super::BuiltinRoutineCatalogEntry;

const fn routine(
    oid: i64,
    name: &'static str,
    source: &'static str,
    argument_types: &'static [i64],
    return_type: i64,
) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "f",
        strict: true,
        volatility: "i",
        parallel: "s",
        leakproof: false,
        return_type,
        argument_types,
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source,
    }
}

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    routine(
        1066,
        "generate_series",
        "generate_series_step_int4",
        &[23, 23, 23],
        23,
    ),
    routine(
        1067,
        "generate_series",
        "generate_series_int4",
        &[23, 23],
        23,
    ),
    routine(
        1068,
        "generate_series",
        "generate_series_step_int8",
        &[20, 20, 20],
        20,
    ),
    routine(
        1069,
        "generate_series",
        "generate_series_int8",
        &[20, 20],
        20,
    ),
    routine(
        3994,
        "generate_series_int4_support",
        "generate_series_int4_support",
        &[2281],
        2281,
    ),
    routine(
        3995,
        "generate_series_int8_support",
        "generate_series_int8_support",
        &[2281],
        2281,
    ),
];
