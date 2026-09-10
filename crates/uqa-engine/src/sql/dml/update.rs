//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE execution, point-update fast paths, and patch eligibility.

use super::{CteScope, Engine, SQLError, SQLParam, SQLResult, UpdatePlan};

pub(in crate::sql) fn run_update(
    engine: &Engine,
    mut stmt: UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_update_inner(engine, &stmt, params)
    })
}

pub(in crate::sql) fn run_update_with_ctes(
    engine: &Engine,
    mut stmt: UpdatePlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_update_inner_with_ctes(engine, &stmt, params, Some(ctes))
    })
}

pub(in crate::sql) fn run_update_inner(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    run_update_inner_with_ctes(engine, stmt, params, None)
}

fn run_update_inner_with_ctes(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<SQLResult, SQLError> {
    if let Some(kind) = super::view_triggers::target_view_kind(engine, &stmt.table)? {
        if kind == crate::StoredViewKind::Materialized {
            let _ = super::view_privileges::ensure_update(engine, stmt)?;
            let relation = crate::RelationIdentity::from_legacy_name(&stmt.table)
                .map_err(SQLError::Internal)?;
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot change materialized view \"{}\"", relation.name),
            });
        }
        if super::view_automatic::has_instead_of_trigger(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerEvent::Update,
        )? || crate::sql::rules::relation_suppresses_original_query(
            engine,
            &stmt.table,
            uqa_sql::ast::RuleEvent::Update,
        )? {
            let _ = super::view_privileges::ensure_update(engine, stmt)?;
            return super::view_triggers::run_view_update_inner(
                engine,
                stmt,
                params,
                inherited_ctes,
            );
        }
        let rewritten =
            super::view_automatic::rewrite_update_to_base(engine, stmt, params, inherited_ctes)?;
        return run_update_inner_with_ctes(engine, &rewritten, params, inherited_ctes);
    }
    uqa_execution::mutation::update::table::run_table_update(
        &engine.statement_execution_context(),
        stmt,
        params,
        inherited_ctes,
    )
}
