//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 array inspection routines and the input, output, binary and statistics identities referenced by array `pg_type` rows.

use super::BuiltinRoutineCatalogEntry;

const fn routine(
    oid: i64,
    name: &'static str,
    return_type: i64,
    argument_types: &'static [i64],
) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "f",
        strict: true,
        volatility: "s",
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

const fn inspection(
    oid: i64,
    name: &'static str,
    return_type: i64,
    argument_types: &'static [i64],
    source: &'static str,
) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        volatility: "i",
        source,
        ..routine(oid, name, return_type, argument_types)
    }
}

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    inspection(747, "array_dims", 25, &[2277], "array_dims"),
    inspection(748, "array_ndims", 23, &[2277], "array_ndims"),
    inspection(2091, "array_lower", 23, &[2277, 23], "array_lower"),
    inspection(2092, "array_upper", 23, &[2277, 23], "array_upper"),
    inspection(2176, "array_length", 23, &[2277, 23], "array_length"),
    inspection(3179, "cardinality", 23, &[2277], "array_cardinality"),
    routine(750, "array_in", 2277, &[2275, 26, 23]),
    routine(751, "array_out", 2275, &[2277]),
    routine(2400, "array_recv", 2277, &[2281, 26, 23]),
    routine(2401, "array_send", 17, &[2277]),
    routine(3816, "array_typanalyze", 16, &[2281]),
];
