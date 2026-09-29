//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `anyenum` I/O, comparison, ordering, hashing and aggregate routines.

use super::BuiltinRoutineCatalogEntry;

const fn function(
    oid: i64,
    name: &'static str,
    strict: bool,
    volatility: &'static str,
    return_type: i64,
    argument_types: &'static [i64],
    source: &'static str,
) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "f",
        strict,
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

const fn aggregate(oid: i64, name: &'static str) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "a",
        strict: false,
        volatility: "i",
        parallel: "s",
        leakproof: false,
        return_type: 3500,
        argument_types: &[3500],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "aggregate_dummy",
    }
}

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    function(
        3414,
        "hashenumextended",
        true,
        "i",
        20,
        &[3500, 20],
        "hashenumextended",
    ),
    function(3506, "enum_in", true, "s", 3500, &[2275, 26], "enum_in"),
    function(3507, "enum_out", true, "s", 2275, &[3500], "enum_out"),
    function(3508, "enum_eq", true, "i", 16, &[3500, 3500], "enum_eq"),
    function(3509, "enum_ne", true, "i", 16, &[3500, 3500], "enum_ne"),
    function(3510, "enum_lt", true, "i", 16, &[3500, 3500], "enum_lt"),
    function(3511, "enum_gt", true, "i", 16, &[3500, 3500], "enum_gt"),
    function(3512, "enum_le", true, "i", 16, &[3500, 3500], "enum_le"),
    function(3513, "enum_ge", true, "i", 16, &[3500, 3500], "enum_ge"),
    function(3514, "enum_cmp", true, "i", 23, &[3500, 3500], "enum_cmp"),
    function(3515, "hashenum", true, "i", 23, &[3500], "hashenum"),
    function(
        3524,
        "enum_smaller",
        true,
        "i",
        3500,
        &[3500, 3500],
        "enum_smaller",
    ),
    function(
        3525,
        "enum_larger",
        true,
        "i",
        3500,
        &[3500, 3500],
        "enum_larger",
    ),
    aggregate(3526, "max"),
    aggregate(3527, "min"),
    function(3528, "enum_first", false, "s", 3500, &[3500], "enum_first"),
    function(3529, "enum_last", false, "s", 3500, &[3500], "enum_last"),
    function(
        3530,
        "enum_range",
        false,
        "s",
        2277,
        &[3500, 3500],
        "enum_range_bounds",
    ),
    function(
        3531,
        "enum_range",
        false,
        "s",
        2277,
        &[3500],
        "enum_range_all",
    ),
    function(3532, "enum_recv", true, "s", 3500, &[2281, 26], "enum_recv"),
    function(3533, "enum_send", true, "s", 17, &[3500], "enum_send"),
];
