//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution for views with `INSTEAD OF` triggers.

use super::{
    resolve_view_target, target_columns, validate_returning_alias_relations,
    with_mutation_snapshot, CteScope, InsertPlan, MutationStatementContext, SQLError, SQLParam,
    SourceOutputPruning,
};
use crate::mutation::output::MutationOutput;
use crate::query::statement::consumer::QueryOutputMode;
use std::{cell::Cell, rc::Rc};

mod rows;
use rows::{ViewInsertRows, ViewInsertSource};

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn run_view_insert_inner<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune_source_outputs: SourceOutputPruning,
    stmt: &InsertPlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let target = resolve_view_target(context.mutation.rules.views, &stmt.table)?;
    if stmt.on_conflict.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "INSERT with ON CONFLICT clause cannot be used with a view".into(),
        });
    }
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    let columns = target_columns(&target, &stmt.columns, "INSERT")?;
    let positions = columns
        .iter()
        .map(|column| {
            target
                .columns
                .iter()
                .position(|candidate| candidate == &column.column)
                .ok_or_else(|| SQLError::UnknownColumn(column.column.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let original_query_survives = !std::iter::once(&target.canonical_name)
        .chain(&stmt.view_rule_relations)
        .try_fold(false, |suppressed, relation| {
            Ok::<_, SQLError>(
                suppressed
                    || uqa_sql::semantics::rules::analysis::relation_suppresses_original_query(
                        context.mutation.rules.rules.analysis,
                        relation,
                        uqa_sql::ast::RuleEvent::Insert,
                    )?,
            )
        })?;
    let has_before_statement_trigger = original_query_survives
        && !context
            .mutation
            .preparation
            .referential
            .triggers
            .catalog
            .triggers_for(
                &target.canonical_name,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Insert,
                false,
                &[],
            )?
            .is_empty();
    let (statement_commands, _running_statement) =
        crate::mutation::statement_end::statement_commands(inherited_ctes);
    let view_statement = crate::mutation::triggers::queue::StatementEvent::new(
        &target.canonical_name,
        uqa_sql::ast::TriggerEvent::Insert,
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
    if original_query_survives {
        statement_commands.after_triggers().fire_before_statement(
            &context.mutation.preparation.referential.triggers,
            &view_statement,
        )?;
    }
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
                        None,
                        control,
                    )
                },
                params,
                ctes,
            )?;
            ctes.scalar_subqueries = stmt.subqueries.as_slice().into();
            let mut has_rules = false;
            for relation in std::iter::once(&target.canonical_name).chain(&stmt.view_rule_relations)
            {
                if !context
                    .mutation
                    .rules
                    .rules
                    .capture_rules(relation, uqa_sql::ast::RuleEvent::Insert)?
                    .is_empty()?
                {
                    has_rules = true;
                    break;
                }
            }
            let query_rules = (has_rules || stmt.rule_values_input.is_some())
                .then(|| {
                    context
                        .mutation
                        .rules
                        .rules
                        .bind_insert_queries(stmt, ctes.privilege_subject()?)
                })
                .transpose()?;
            if let Some(queries) = &query_rules {
                if queries.original.is_none() {
                    return crate::mutation::rules::run_insert_query_rules(
                        &context.mutation.rules.rules,
                        context.mutation.preparation.returning,
                        queries,
                        params,
                        ctes,
                        prune_source_outputs,
                    );
                }
            }
            let row_output = Rc::new(ViewInsertRows::new(
                context,
                read_context,
                &target,
                stmt,
                params,
                ctes,
            )?);
            if let Some(source) = stmt.source.as_deref().or_else(|| {
                query_rules
                    .as_ref()
                    .and_then(|queries| queries.original.as_deref())
            }) {
                let source = query_rules
                    .as_ref()
                    .and_then(|queries| queries.original.as_deref())
                    .unwrap_or(source);
                let mut source_scope = ctes.returning_statement_snapshot_scope();
                source_scope.enable_command_progress_streaming();
                crate::query::statement::execute_query_plan_output(
                    &read_context.query,
                    source,
                    params,
                    &mut source_scope,
                    QueryOutputMode::physical_consumer(Rc::new(ViewInsertSource {
                        rows: Rc::clone(&row_output),
                        columns: &columns,
                        positions: &positions,
                        width: Cell::new(0),
                    })),
                )?;
            } else {
                row_output.values(&columns, &positions, &stmt.rows)?;
            }
            crate::mutation::statement_end::end_command(
                &statement_commands,
                &context.mutation.preparation.referential.triggers,
                std::slice::from_ref(&view_statement),
                crate::mutation::triggers::events::AfterRowEvents::default(),
            )?;
            if let Some(queries) = &query_rules {
                crate::mutation::rules::run_insert_query_rules(
                    &context.mutation.rules.rules,
                    context.mutation.preparation.returning,
                    queries,
                    params,
                    ctes,
                    prune_source_outputs,
                )?;
            }
            row_output.finish()
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
