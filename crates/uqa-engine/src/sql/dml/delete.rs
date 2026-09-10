//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE candidate selection, command policy, staging, and publication.

use super::{
    validate_returning_alias_relations, BTreeSet, CteScope, DeletePlan, DocId, Engine,
    PreparedDocumentDelete, SQLError, SQLParam, SQLResult,
};

pub(in crate::sql) fn run_delete(
    engine: &Engine,
    mut stmt: DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    super::run_mutation_command(engine, move |engine| {
        run_delete_inner(engine, &stmt, params)
    })
}

pub(in crate::sql) fn run_delete_with_ctes(
    engine: &Engine,
    mut stmt: DeletePlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_delete_inner_with_ctes(engine, &stmt, params, Some(ctes))
    })
}

pub(in crate::sql) fn run_delete_inner(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    run_delete_inner_with_ctes(engine, stmt, params, None)
}

fn run_delete_inner_with_ctes(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<SQLResult, SQLError> {
    if let Some(kind) = super::view_triggers::target_view_kind(engine, &stmt.table)? {
        if kind == crate::StoredViewKind::Materialized {
            let _ = super::view_privileges::ensure_delete(engine, stmt)?;
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
            uqa_sql::ast::TriggerEvent::Delete,
        )? || crate::sql::rules::relation_suppresses_original_query(
            engine,
            &stmt.table,
            uqa_sql::ast::RuleEvent::Delete,
        )? {
            let _ = super::view_privileges::ensure_delete(engine, stmt)?;
            return super::view_triggers::run_view_delete_inner(
                engine,
                stmt,
                params,
                inherited_ctes,
            );
        }
        let rewritten =
            super::view_automatic::rewrite_delete_to_base(engine, stmt, params, inherited_ctes)?;
        return run_delete_inner_with_ctes(engine, &rewritten, params, inherited_ctes);
    }
    uqa_execution::mutation::delete::run_table_delete(
        &engine.statement_execution_context(),
        stmt,
        params,
        inherited_ctes,
    )
}

pub(in crate::sql) fn prepare_document_delete(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    referential_actions: &mut super::ReferentialActionContext,
    fire_row_triggers: bool,
) -> Result<Option<PreparedDocumentDelete>, SQLError> {
    uqa_execution::mutation::referential::prepare_document_delete(
        &engine.referential_execution_context(),
        table,
        doc_id,
        params,
        root_deletes,
        referential_actions,
        fire_row_triggers,
    )
}
pub(in crate::sql) fn stage_prepared_document_delete(
    engine: &Engine,
    prepared: &mut PreparedDocumentDelete,
    params: &[SQLParam],
    after_row_events: &mut Vec<crate::sql::triggers::AfterRowTriggerEvent>,
) -> Result<(), SQLError> {
    uqa_execution::mutation::staging::stage_prepared_document_delete(
        engine.mutation_staging_context(),
        prepared,
        params,
        after_row_events,
    )
}
