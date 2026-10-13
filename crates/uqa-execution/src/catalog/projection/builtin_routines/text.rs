//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared text concatenation signatures and their `PostgreSQL` identities.

use super::BuiltinRoutineCatalogEntry;

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    BuiltinRoutineCatalogEntry {
        oid: 3058,
        name: "concat",
        kind: "f",
        strict: false,
        volatility: "s",
        parallel: "s",
        leakproof: false,
        return_type: 25,
        argument_types: &[2276],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "text_concat",
    },
    BuiltinRoutineCatalogEntry {
        oid: 3059,
        name: "concat_ws",
        kind: "f",
        strict: false,
        volatility: "s",
        parallel: "s",
        leakproof: false,
        return_type: 25,
        argument_types: &[25, 2276],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "text_concat_ws",
    },
];
