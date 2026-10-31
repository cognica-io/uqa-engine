//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE execution for views with INSTEAD OF triggers.

use super::{
    build_join_spill_with_ctes, build_returning_value_row, finish_view_dml,
    matching_source_context, materialize_view_rows, resolve_view_target, target_row,
    validate_dml_expression_qualifiers, validate_returning_alias_relations, view_document,
    view_values, with_mutation_snapshot, BTreeSet, CteScope, DeletePlan, DmlReturningShape,
    MutationImageBuffer, MutationStatementContext, ReturningValueProjectionRow, SQLError, SQLParam,
    ViewDmlSourceMatch,
};
use crate::mutation::output::MutationOutput;

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn run_view_delete_inner<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    stmt: &DeletePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let target = resolve_view_target(context, &stmt.table)?;
    if stmt.source.is_none() {
        let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
        if let Some(predicate) = stmt.predicate.as_ref() {
            validate_dml_expression_qualifiers(predicate, &allowed)?;
        }
    }
    let has_before_statement_trigger = !context
        .mutation
        .preparation
        .referential
        .triggers
        .catalog
        .triggers_for(
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Delete,
            false,
            &[],
        )?
        .is_empty();
    let (statement_commands, _running_statement) =
        crate::mutation::statement_end::statement_commands(
            context.query.source.ctes,
            inherited_ctes,
        )?;
    let view_statement = crate::mutation::triggers::queue::StatementEvent::new(
        &target.canonical_name,
        uqa_sql::ast::TriggerEvent::Delete,
        &[],
    );
    let statement_snapshot = match inherited_ctes.and_then(CteScope::command_cte_snapshot) {
        Some(snapshot) => Some(snapshot),
        None if has_before_statement_trigger
            || stmt.ctes.iter().any(|cte| cte.body.modifies_data()) =>
        {
            Some(std::sync::Arc::new(context.snapshots.capture()?))
        }
        None => None,
    };

    statement_commands.after_triggers().fire_before_statement(
        &context.mutation.preparation.referential.triggers,
        &view_statement,
    )?;

    let mut statement_scope = None;
    let mut execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<MutationOutput, SQLError> {
            let ctes = statement_scope.insert(read_context.mutation.scopes.command_scope(
                stmt.statement_privilege_subject.as_ref(),
                stmt.relations_bound,
            )?);
            if let Some(parent) = inherited_ctes {
                ctes.inherit_cte_bindings(parent);
            }
            ctes.set_statement_commands(std::sync::Arc::clone(&statement_commands));
            ctes.set_command_cte_snapshot(statement_snapshot.clone());
            crate::query::cte::materialize_command_ctes(
                context.query.source.ctes,
                &stmt.ctes,
                |control| {
                    uqa_sql::semantics::primary_command_cte_references_with_control(
                        &stmt.ctes,
                        stmt.query_inputs(),
                        stmt.source_input(),
                        control,
                    )
                },
                params,
                ctes,
            )?;
            let arena_resources = context.query.source.ctes.production_resources()?;
            ctes.scalar_subqueries = uqa_sql::plan::RetainedQueryArena::capture(
                &stmt.subqueries,
                &arena_resources.control(),
            )?;
            let mut source_scope = ctes.returning_statement_snapshot_scope();
            let source_privilege_expressions = stmt
                .predicate
                .iter()
                .chain(stmt.returning.iter().map(|projection| &projection.expr))
                .collect::<Vec<_>>();
            if let Some(source) = stmt.source.as_deref() {
                crate::query::privileges::ensure_select_privileges_for_source_expressions(
                    source,
                    &source_privilege_expressions,
                    &source_scope,
                )?;
            }
            let source_rows = stmt
                .source
                .as_deref()
                .map(|source| {
                    build_join_spill_with_ctes(
                        &read_context.query.source,
                        source,
                        params,
                        &mut source_scope,
                    )
                })
                .transpose()?;
            validate_returning_alias_relations(
                &stmt.target_qualifier,
                &stmt.returning_aliases,
                source_rows.as_ref().map(crate::SharedSpill::row_schema),
            )?;
            let mut target_scope = ctes.returning_statement_snapshot_scope();
            let candidates =
                materialize_view_rows(&read_context.query, &target, params, &mut target_scope)?;
            let snapshot = ctes.returning_statement_snapshot_scope();
            let cancellation = context.mutation.rules.rules.cancellation.clone();
            let mut pending = MutationImageBuffer::new(
                context.mutation.rules.rules.memory.work_mem_bytes()?,
                cancellation.clone(),
            );
            for candidate in candidates
                .read_rows()
                .map_err(crate::physical::physical_exec_error)?
            {
                cancellation.check()?;
                let old = candidate
                    .map_err(crate::physical::physical_exec_error)?
                    .row
                    .into_physical_values();
                let physical = target_row(&target, &stmt.target_qualifier, &old)?;
                let Some(source_match) = matching_source_context(
                    read_context
                        .mutation
                        .preparation
                        .referential
                        .assignment
                        .expressions,
                    &physical,
                    source_rows.as_ref(),
                    stmt.predicate.as_ref(),
                    params,
                    &snapshot,
                )?
                else {
                    continue;
                };
                let source_context = match source_match {
                    ViewDmlSourceMatch::TargetOnly => None,
                    ViewDmlSourceMatch::Source(source) => Some(source),
                };
                pending.push(crate::mutation::row_images::OwnedMutationImages {
                    old_storage_table: None,
                    old_doc_id: None,
                    old: Some(view_document(&target, &old)?),
                    new_storage_table: None,
                    new_doc_id: None,
                    new: None,
                    context: source_context,
                })?;
            }
            let pending = pending.finish();
            drop(candidates);
            let mut affected = 0_u64;
            let mut returning_rows = crate::mutation::returning::ReturningRows::new(
                context.query.source.relational.runtime,
            )?;
            for index in 0..pending.len() {
                let row = pending.get(index)?;
                let old = view_values(&target, row.old)?;
                let source_context = row.context;
                if crate::mutation::triggers::fire_instead_of_row_triggers(
                    &context.mutation.preparation.referential.triggers,
                    &target.canonical_name,
                    uqa_sql::ast::TriggerEvent::Delete,
                    Some(&old),
                    None,
                    &[],
                )?
                .is_none()
                {
                    continue;
                }
                affected += 1;
                if !stmt.returning.is_empty() {
                    returning_rows.push_to(
                        build_returning_value_row(
                            context.mutation.preparation.returning,
                            ReturningValueProjectionRow {
                                table: &target.canonical_name,
                                target_qualifier: &stmt.target_qualifier,
                                current: &old,
                                old: Some(&old),
                                new: None,
                                aliases: &stmt.returning_aliases,
                                context: source_context.as_ref(),
                            },
                            &stmt.returning,
                            params,
                            ctes,
                        )?,
                        context.mutation.preparation.returning.destination,
                    )?;
                }
            }

            crate::mutation::statement_end::end_command(
                &statement_commands,
                &context.mutation.preparation.referential.triggers,
                std::slice::from_ref(&view_statement),
                crate::mutation::triggers::events::AfterRowEvents::default(),
            )?;

            let result = finish_view_dml(
                &context.mutation.preparation.returning,
                DmlReturningShape {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes,
                    supplemental_schema: source_rows.as_ref().map(crate::SharedSpill::row_schema),
                },
                returning_rows,
                affected,
            )?;
            Ok(result)
        };
    let result = match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    };
    crate::mutation::statement_end::finish_statement(
        context,
        params,
        &stmt.ctes,
        statement_scope.as_mut(),
        result,
    )
}
