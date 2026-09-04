//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 asynchronous-notification routine metadata.

use super::BuiltinRoutineCatalogEntry;

pub(super) const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    BuiltinRoutineCatalogEntry {
        oid: 2026,
        name: "pg_backend_pid",
        kind: "f",
        strict: true,
        volatility: "s",
        parallel: "r",
        leakproof: false,
        return_type: 23,
        argument_types: &[],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "pg_backend_pid",
    },
    BuiltinRoutineCatalogEntry {
        oid: 3035,
        name: "pg_listening_channels",
        kind: "f",
        strict: true,
        volatility: "s",
        parallel: "r",
        leakproof: false,
        return_type: 25,
        argument_types: &[],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "pg_listening_channels",
    },
    BuiltinRoutineCatalogEntry {
        oid: 3036,
        name: "pg_notify",
        kind: "f",
        strict: false,
        volatility: "v",
        parallel: "r",
        leakproof: false,
        return_type: 2278,
        argument_types: &[25, 25],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "pg_notify",
    },
    BuiltinRoutineCatalogEntry {
        oid: 3296,
        name: "pg_notification_queue_usage",
        kind: "f",
        strict: true,
        volatility: "v",
        parallel: "r",
        leakproof: false,
        return_type: 701,
        argument_types: &[],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "pg_notification_queue_usage",
    },
];
