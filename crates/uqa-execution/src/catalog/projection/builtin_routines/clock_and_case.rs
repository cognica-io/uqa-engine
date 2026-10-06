//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 clock and text case-conversion routine identities.

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
    routine(274, "timeofday", "v", 25, &[], "timeofday"),
    routine(870, "lower", "i", 25, &[25], "lower"),
    routine(871, "upper", "i", 25, &[25], "upper"),
    routine(1299, "now", "s", 1184, &[], "now"),
    routine(2647, "transaction_timestamp", "s", 1184, &[], "now"),
    routine(
        2648,
        "statement_timestamp",
        "s",
        1184,
        &[],
        "statement_timestamp",
    ),
    routine(2649, "clock_timestamp", "v", 1184, &[], "clock_timestamp"),
];
