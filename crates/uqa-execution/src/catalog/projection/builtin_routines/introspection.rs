//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog identities of SQL type-introspection routines.

use super::BuiltinRoutineCatalogEntry;

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[BuiltinRoutineCatalogEntry {
    oid: 1619,
    name: "pg_typeof",
    kind: "f",
    strict: false,
    volatility: "s",
    parallel: "s",
    leakproof: false,
    return_type: 2206,
    argument_types: &[2276],
    argument_names: &[],
    default_arguments: 0,
    argument_defaults: None,
    source: "pg_typeof",
}];
