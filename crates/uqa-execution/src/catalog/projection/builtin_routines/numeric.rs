//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 internal remainder, power and root routine identities.

use super::BuiltinRoutineCatalogEntry;

const fn routine(
    oid: i64,
    name: &'static str,
    return_type: i64,
    argument_types: &'static [i64],
    source: &'static str,
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
    routine(940, "mod", 21, &[21, 21], "int2mod"),
    routine(941, "mod", 23, &[23, 23], "int4mod"),
    routine(947, "mod", 20, &[20, 20], "int8mod"),
    routine(1344, "sqrt", 701, &[701], "dsqrt"),
    routine(1345, "cbrt", 701, &[701], "dcbrt"),
    routine(1346, "pow", 701, &[701, 701], "dpow"),
    routine(1368, "power", 701, &[701, 701], "dpow"),
    routine(1728, "mod", 1700, &[1700, 1700], "numeric_mod"),
    routine(1730, "sqrt", 1700, &[1700], "numeric_sqrt"),
    routine(1738, "pow", 1700, &[1700, 1700], "numeric_power"),
    routine(2169, "power", 1700, &[1700, 1700], "numeric_power"),
];
