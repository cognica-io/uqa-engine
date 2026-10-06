//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog identities of the existing SUM and window execution kernels.

use super::BuiltinRoutineCatalogEntry;

macro_rules! routine {
    ($oid:expr, $name:literal, $kind:literal, $strict:expr, $result:expr, $args:expr, $source:literal) => {
        BuiltinRoutineCatalogEntry {
            oid: $oid,
            name: $name,
            kind: $kind,
            strict: $strict,
            volatility: "i",
            parallel: "s",
            leakproof: false,
            return_type: $result,
            argument_types: $args,
            argument_names: &[],
            default_arguments: 0,
            argument_defaults: None,
            source: $source,
        }
    };
}

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    routine!(2107, "sum", "a", false, 1700, &[20], "aggregate_dummy"),
    routine!(2108, "sum", "a", false, 20, &[23], "aggregate_dummy"),
    routine!(2109, "sum", "a", false, 20, &[21], "aggregate_dummy"),
    routine!(2110, "sum", "a", false, 700, &[700], "aggregate_dummy"),
    routine!(2111, "sum", "a", false, 701, &[701], "aggregate_dummy"),
    routine!(2113, "sum", "a", false, 1186, &[1186], "aggregate_dummy"),
    routine!(2114, "sum", "a", false, 1700, &[1700], "aggregate_dummy"),
    routine!(3100, "row_number", "w", false, 20, &[], "window_row_number"),
    routine!(3101, "rank", "w", false, 20, &[], "window_rank"),
    routine!(3102, "dense_rank", "w", false, 20, &[], "window_dense_rank"),
    routine!(
        3103,
        "percent_rank",
        "w",
        false,
        701,
        &[],
        "window_percent_rank"
    ),
    routine!(3104, "cume_dist", "w", false, 701, &[], "window_cume_dist"),
    routine!(3105, "ntile", "w", true, 23, &[23], "window_ntile"),
    routine!(3106, "lag", "w", true, 2283, &[2283], "window_lag"),
    routine!(
        3107,
        "lag",
        "w",
        true,
        2283,
        &[2283, 23],
        "window_lag_with_offset"
    ),
    routine!(
        3108,
        "lag",
        "w",
        true,
        5077,
        &[5077, 23, 5077],
        "window_lag_with_offset_and_default"
    ),
    routine!(3109, "lead", "w", true, 2283, &[2283], "window_lead"),
    routine!(
        3110,
        "lead",
        "w",
        true,
        2283,
        &[2283, 23],
        "window_lead_with_offset"
    ),
    routine!(
        3111,
        "lead",
        "w",
        true,
        5077,
        &[5077, 23, 5077],
        "window_lead_with_offset_and_default"
    ),
    routine!(
        3112,
        "first_value",
        "w",
        true,
        2283,
        &[2283],
        "window_first_value"
    ),
    routine!(
        3113,
        "last_value",
        "w",
        true,
        2283,
        &[2283],
        "window_last_value"
    ),
    routine!(
        3114,
        "nth_value",
        "w",
        true,
        2283,
        &[2283, 23],
        "window_nth_value"
    ),
    routine!(
        6233,
        "window_row_number_support",
        "f",
        true,
        2281,
        &[2281],
        "window_row_number_support"
    ),
    routine!(
        6234,
        "window_rank_support",
        "f",
        true,
        2281,
        &[2281],
        "window_rank_support"
    ),
    routine!(
        6235,
        "window_dense_rank_support",
        "f",
        true,
        2281,
        &[2281],
        "window_dense_rank_support"
    ),
    routine!(
        6306,
        "window_percent_rank_support",
        "f",
        true,
        2281,
        &[2281],
        "window_percent_rank_support"
    ),
    routine!(
        6307,
        "window_cume_dist_support",
        "f",
        true,
        2281,
        &[2281],
        "window_cume_dist_support"
    ),
    routine!(
        6308,
        "window_ntile_support",
        "f",
        true,
        2281,
        &[2281],
        "window_ntile_support"
    ),
];
