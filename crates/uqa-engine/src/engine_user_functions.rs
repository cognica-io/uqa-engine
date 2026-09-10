//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registry for user-defined SQL / `PL/pgSQL` routines (`CREATE
//! FUNCTION` / `CREATE PROCEDURE`). Definitions persist to catalog
//! metadata like views and sequences; the compiled body is rebuilt
//! from the definition at registration and restore time.

mod combined_overloads;
mod declaration;
pub(crate) use declaration::resolve_plpgsql_datum_types;
mod lifecycle;
mod resolution;
mod security;

pub(crate) use resolution::{
    routine_local_name, routine_returns_anonymous_record, routine_signature_types, RoutineCallKind,
    RoutineResolution,
};
pub(crate) use uqa_execution::canonical_routine_type_name;

pub(crate) fn builtin_routine_support_oid(name: &str) -> Option<i64> {
    Some(match name.strip_prefix("pg_catalog.").unwrap_or(name) {
        "textlike_support" => 1023,
        "texticregexeq_support" => 1024,
        "texticlike_support" => 1025,
        "network_subset_support" => 1173,
        "textregexeq_support" => 1364,
        "varchar_support" => 3097,
        "numeric_support" => 3157,
        _ => return None,
    })
}

pub(crate) use uqa_sql::routines::{
    is_routine_namespace_lookup_error, CompiledFunctionBody, SQLUserFunction,
};
