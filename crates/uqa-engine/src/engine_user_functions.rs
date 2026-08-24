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

pub(crate) use resolution::{
    routine_local_name, routine_returns_anonymous_record, routine_signature_types, RoutineCallKind,
};
pub(crate) use uqa_execution::canonical_routine_type_name;
use uqa_planner::UnifiedPlan;
use uqa_sql::ast::CreateFunction;

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
