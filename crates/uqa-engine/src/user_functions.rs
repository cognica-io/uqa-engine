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
};
pub(crate) use uqa_execution::canonical_routine_type_name;

pub(crate) use uqa_sql::routines::builtin_routine_support_oid;

pub(crate) use uqa_sql::routines::{
    is_routine_namespace_lookup_error, CompiledFunctionBody, SQLUserFunction,
};
