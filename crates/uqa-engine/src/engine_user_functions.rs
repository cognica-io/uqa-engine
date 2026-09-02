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
mod lifecycle;
mod resolution;
mod security;

pub(crate) use resolution::{
    routine_local_name, routine_returns_anonymous_record, routine_signature_types, RoutineCallKind,
    RoutineResolution,
};
pub(crate) use uqa_execution::canonical_routine_type_name;
use uqa_planner::UnifiedPlan;
use uqa_sql::{ast::CreateFunction, SQLError};

pub(crate) fn is_routine_namespace_lookup_error(error: &SQLError) -> bool {
    matches!(
        error,
        SQLError::Routine { sqlstate, message }
            if sqlstate == "3F000"
                || (sqlstate == "42501"
                    && message.starts_with("permission denied for schema "))
    )
}

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

/// A registered routine: the persistable definition plus its
/// pre-compiled body.
#[derive(Clone)]
pub(crate) struct SQLUserFunction {
    pub def: CreateFunction,
    pub compiled: CompiledFunctionBody,
}

/// Executable form of a routine body.
// The project naming convention spells the acronym as `SQL`.
#[allow(clippy::upper_case_acronyms)]
#[derive(Clone)]
pub(crate) enum CompiledFunctionBody {
    PLpgSQL(uqa_sql::plpgsql::PLpgSQLFunction),
    SQL(Vec<UnifiedPlan>),
}
