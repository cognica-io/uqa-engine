//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 temporal truncation and interval normalization identities.

use super::BuiltinRoutineCatalogEntry;

const fn routine(
    oid: i64,
    name: &'static str,
    volatility: &'static str,
    return_type: i64,
    argument_types: &'static [i64],
    source: &'static str,
) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "f",
        strict: true,
        volatility,
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
        1175,
        "justify_hours",
        "i",
        1186,
        &[1186],
        "interval_justify_hours",
    ),
    routine(
        1217,
        "date_trunc",
        "s",
        1184,
        &[25, 1184],
        "timestamptz_trunc",
    ),
    routine(1218, "date_trunc", "i", 1186, &[25, 1186], "interval_trunc"),
    routine(
        1284,
        "date_trunc",
        "i",
        1184,
        &[25, 1184, 25],
        "timestamptz_trunc_zone",
    ),
    routine(
        1295,
        "justify_days",
        "i",
        1186,
        &[1186],
        "interval_justify_days",
    ),
    routine(
        2020,
        "date_trunc",
        "i",
        1114,
        &[25, 1114],
        "timestamp_trunc",
    ),
    routine(
        2711,
        "justify_interval",
        "i",
        1186,
        &[1186],
        "interval_justify_interval",
    ),
];
