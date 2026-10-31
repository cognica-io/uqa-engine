//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table INSERT scheduling, snapshot reads, row staging, and publication.

mod query;
use super::{
    codec::{
        decode_prepared_insert_spill_row, prepared_insert_spill_schema,
        push_prepared_insert_spill_row, PreparedInsertSpillRow,
    },
    known_new::KnownNewInserts,
    rows::prepare_values_insert_row,
    source::{InsertSelectConsumer, InsertSelectIdentity, PreparedInsertSelect},
    supplied_identities::SuppliedIdentities,
    triggers::insert_statement_events,
};
use crate::mutation::output::MutationOutput;
use crate::mutation::statement::context::{with_mutation_snapshot, MutationStatementContext};
use crate::{
    mutation::{
        assignment::{
            apply_missing_column_defaults, eval_mutation_assignment, MutationAssignmentTarget,
        },
        command_scope::MutationOverlayScope,
        conflict::update::InsertConflictLocks,
        constraints::{partition_insert_target, ConstraintStatement},
        identity::{
            insert_identity_columns, persist_auto_increment_identity,
            prepare_auto_increment_identity, prepare_insert_identity,
        },
        publication::{apply_validated_prepared_insert, finish_mutation_publication},
        returning::{dml_returning_result, DmlReturningShape},
        statement_end,
        triggers::queue::StatementEvent,
    },
    query::CteScope,
};
use std::{rc::Rc, sync::Arc};
use uqa_sql::assignment::columns::validate_mutation_targets_with_control;
use uqa_sql::{
    plan::{ConflictActionPlan, ConflictPlan, InsertPlan, QueryPlan},
    semantics::{
        conflict::InferenceContext,
        returning::{validate_returning_alias_relations, ReturningAnalysisContext},
    },
    SQLError, SQLParam,
};
use uqa_storage::document_store::Document;

/// SQL analysis inputs and the planner's positional source-output rewrite for one INSERT.
pub struct InsertPlanning<'a> {
    pub inference: InferenceContext<'a>,
    pub returning: ReturningAnalysisContext<'a>,
    pub prune_source_outputs: crate::mutation::views::commands::SourceOutputPruning,
}

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub fn run_table_insert<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    planning: InsertPlanning<'_>,
    stmt: &InsertPlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let mutation = &context.mutation;
    let preparation = mutation.preparation;
    let assignment = preparation.referential.assignment;
    let triggers = preparation.referential.triggers;
    let (statement_commands, _running_statement) =
        statement_end::statement_commands(context.query.source.ctes, inherited_ctes)?;
    let _trigger_scope = crate::mutation::triggers::TriggerStatementScope::enter();
    preparation.referential.locking.session.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    let insert_rules = mutation
        .rules
        .rules
        .capture_rules(&stmt.table, uqa_sql::ast::RuleEvent::Insert)?;
    let has_insert_rules = !insert_rules.is_empty()?;
    let has_view_insert_rules = !stmt.view_rule_relations.is_empty();
    let has_any_insert_rules = has_insert_rules || has_view_insert_rules;
    if stmt.on_conflict.is_some()
        && (has_insert_rules
            || !mutation
                .rules
                .rules
                .capture_rules(&stmt.table, uqa_sql::ast::RuleEvent::Update)?
                .is_empty()?)
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "INSERT with ON CONFLICT clause cannot be used with table that has INSERT or UPDATE rules".into(),
        });
    }
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    uqa_sql::semantics::rules::validate_rule_returning_contract(
        mutation.rules.rules.analysis.rules,
        &stmt.table,
        uqa_sql::ast::RuleEvent::Insert,
        !stmt.returning.is_empty(),
    )?;
    if let Some(view_returning) = &stmt.view_rule_returning {
        uqa_sql::semantics::rules::validate_rule_returning_contract(
            mutation.rules.rules.analysis.rules,
            &view_returning.relation,
            uqa_sql::ast::RuleEvent::Insert,
            !view_returning.returning.is_empty(),
        )?;
    }
    let resources = context.query.source.ctes.production_resources()?;
    let control = resources.control();
    let prepared_statement =
        uqa_sql::semantics::conflict::prepare_inference_predicate_with_control(
            planning.inference,
            stmt,
            params,
            &control,
        )?;
    let stmt = prepared_statement.as_ref().map_or(stmt, |command| {
        let uqa_sql::plan::CommandPlan::Insert(insert) = &**command else {
            unreachable!("INSERT inference")
        };
        insert
    });
    if let Some(conflict) = stmt.on_conflict.as_ref() {
        uqa_sql::semantics::conflict::validate_conflict_target(
            planning.inference.catalog,
            &stmt.table,
            conflict,
        )?;
    }
    let conflict_update_columns_owner = if let Some(ConflictPlan {
        action: ConflictActionPlan::Update { assignments, .. },
        ..
    }) = stmt.on_conflict.as_ref()
    {
        validate_mutation_targets_with_control(
            assignment.columns,
            &stmt.table,
            assignments
                .iter()
                .flat_map(|assignment| assignment.target.targets()),
            "INSERT ON CONFLICT DO UPDATE",
            false,
            &control,
        )?;
        Some(
            uqa_sql::semantics::mutation_privileges::assignment_column_names_with_control(
                assignments,
                &control,
            )?,
        )
    } else {
        None
    };
    let conflict_update_columns = conflict_update_columns_owner
        .as_ref()
        .map(|columns| columns.as_slice());
    if stmt.view_rule_relations.is_empty() && !stmt.columns.is_empty() {
        validate_mutation_targets_with_control(
            assignment.columns,
            &stmt.table,
            stmt.columns.iter(),
            "INSERT",
            true,
            &control,
        )?;
    }
    if stmt.view_rule_relations.is_empty() {
        super::identity_targets::validate_insert_identity_targets(
            mutation.identities,
            stmt,
            &control,
        )?;
    }
    uqa_sql::semantics::returning::validate_insert_returning(
        planning.returning,
        stmt,
        params,
        inherited_ctes.map(|scope| scope as &dyn uqa_sql::semantics::returning::ReturningScope),
    )?;
    uqa_sql::semantics::mutation_privileges::ensure_insert_target_privileges_with_control(
        mutation.privileges,
        stmt,
        conflict_update_columns,
        &control,
    )?;
    // The columns the statement supplies, which its constraint violations show to a role that may not read the table.
    let supplied_columns =
        uqa_sql::semantics::mutation_privileges::insert_constraint_columns_with_control(
            mutation.privileges,
            stmt,
            conflict_update_columns,
            &control,
        )?;
    let statement_relation = crate::mutation::constraints::statement_relation(
        preparation.referential.constraints,
        &stmt.table,
    )?;
    let statement = ConstraintStatement::new(&statement_relation, &supplied_columns);
    let view_original_query = !stmt.view_rule_relations.iter().try_fold(
        false,
        |suppressed, relation| -> Result<bool, SQLError> {
            Ok(suppressed
                || mutation
                    .rules
                    .rules
                    .capture_rules(relation, uqa_sql::ast::RuleEvent::Insert)?
                    .any(|rule| rule.definition.instead && rule.definition.condition.is_none())?)
        },
    )?;
    let insert_original_query = view_original_query
        && !insert_rules
            .any(|rule| rule.definition.instead && rule.definition.condition.is_none())?;
    if insert_original_query {
        if !stmt.view_rule_relations.is_empty() {
            super::identity_targets::validate_insert_identity_targets(
                mutation.identities,
                stmt,
                &control,
            )?;
        }
        crate::mutation::routine_calls::insert(context, stmt, params, inherited_ctes)?;
    }
    let has_before_insert_statement_trigger = insert_original_query
        && !triggers
            .catalog
            .triggers_for(
                &stmt.table,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Insert,
                false,
                &[],
            )?
            .is_empty();
    let has_before_update_statement_trigger = if let Some(columns) = conflict_update_columns {
        !triggers
            .catalog
            .triggers_for(
                &stmt.table,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Update,
                false,
                columns,
            )?
            .is_empty()
    } else {
        false
    };
    let statement_snapshot = match inherited_ctes.and_then(CteScope::command_cte_snapshot) {
        Some(snapshot) => Some(snapshot),
        None if has_before_insert_statement_trigger
            || has_before_update_statement_trigger
            || stmt.ctes.iter().any(|cte| cte.body.modifies_data()) =>
        {
            Some(std::sync::Arc::new(context.snapshots.capture()?))
        }
        None => None,
    };
    if insert_original_query {
        statement_commands.after_triggers().fire_before_statement(
            &triggers,
            &StatementEvent::new(&stmt.table, uqa_sql::ast::TriggerEvent::Insert, &[]),
        )?;
    }
    if let Some(columns) = conflict_update_columns {
        statement_commands.after_triggers().fire_before_statement(
            &triggers,
            &StatementEvent::new(&stmt.table, uqa_sql::ast::TriggerEvent::Update, columns),
        )?;
    }
    let mut statement_scope = None;
    let mut execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<MutationOutput, SQLError> {
            let read_assignment = read_context.mutation.preparation.referential.assignment;
            let scope = statement_scope.insert(read_context.mutation.scopes.command_scope(
                stmt.statement_privilege_subject.as_ref(),
                stmt.relations_bound,
            )?);
            if let Some(parent) = inherited_ctes {
                scope.inherit_cte_bindings(parent);
            }
            scope.set_statement_commands(Arc::clone(&statement_commands));
            scope.set_command_cte_snapshot(statement_snapshot.clone());
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
                scope,
            )?;
            let arena_resources = context.query.source.ctes.production_resources()?;
            scope.scalar_subqueries = uqa_sql::plan::RetainedQueryArena::capture(
                &stmt.subqueries,
                &arena_resources.control(),
            )?;
            let query_rules = (has_any_insert_rules || stmt.rule_values_input.is_some())
                .then(|| {
                    mutation
                        .rules
                        .rules
                        .bind_insert_queries(stmt, scope.privilege_subject()?)
                })
                .transpose()?;
            if let Some(queries) = &query_rules {
                if queries.original.is_none() {
                    return crate::mutation::rules::run_insert_query_rules(
                        &mutation.rules.rules,
                        preparation.returning,
                        queries,
                        params,
                        scope,
                        planning.prune_source_outputs,
                    );
                }
            }
            // Resolve the column that names an inserted row: the table's single PRIMARY KEY column, whether a sequence generates its values or not, and otherwise the conventional legacy `id` slot of a table without declared columns. Both VALUES and SELECT sources must derive the internal doc id from this same column or later primary-key rewrites can address a different row than the one that was inserted.
            let (auto_id_col, id_column, identity_source) =
                insert_identity_columns(mutation.identities, &stmt.table, "INSERT")?;
            // INSERT ... SELECT: the query executor feeds each positional physical row directly into the INSERT sink. Ordinary source scans and scalar subqueries retain the statement snapshot, while a VOLATILE callback observes the logical mutations staged by preceding rows of this command.
            if let Some(source) = stmt.source.as_deref().or_else(|| {
                query_rules
                    .as_ref()
                    .and_then(|queries| queries.original.as_deref())
            }) {
                let output = query::run(query::InsertQueryExecution {
                    context,
                    read_context,
                    stmt,
                    source: query_rules
                        .as_ref()
                        .and_then(|queries| queries.original.as_deref())
                        .unwrap_or(source),
                    params,
                    scope,
                    identity: InsertSelectIdentity {
                        auto_id_column: auto_id_col,
                        id_column,
                        identity_source,
                    },
                    conflict_update_columns,
                    statement_commands: &statement_commands,
                    control,
                })?;
                if let Some(queries) = &query_rules {
                    crate::mutation::rules::run_insert_query_rules(
                        &mutation.rules.rules,
                        preparation.returning,
                        queries,
                        params,
                        scope,
                        planning.prune_source_outputs,
                    )?;
                }
                return Ok(output);
            }

            let implicit_columns = stmt.columns.is_empty();
            let owned_columns;
            let columns: &[uqa_sql::ast::AssignmentTarget<uqa_sql::ScalarExpr>] =
                if implicit_columns {
                    let mut targets = uqa_core::memory::ProductionVec::new(control);
                    preparation
                        .returning
                        .catalog
                        .visit_column_names_with_control(&stmt.table, &control, &mut |name| {
                            targets.push_produced(
                                uqa_sql::assignment::targets::whole_column_with_control(
                                    name, &control,
                                )?,
                            )?;
                            Ok(())
                        })
                        .map_err(|error| {
                            uqa_sql::catalog::errors::dml_storage_error_with_control(
                                "INSERT", error, &control,
                            )
                        })?;
                    owned_columns = targets.finish()?;
                    &owned_columns
                } else {
                    &stmt.columns
                };
            if view_original_query {
                validate_mutation_targets_with_control(
                    assignment.columns,
                    &stmt.table,
                    columns.iter(),
                    "INSERT",
                    true,
                    &control,
                )?;
            }

            // No explicit id and no auto-increment column: allocate a synthetic u64 doc_id at insert time. Every table has an implicit doc_id even when the schema declares no primary key.

            let mut affected = 0u64;
            let mut returning_rows = crate::mutation::returning::ReturningRows::new(
                context.query.source.relational.runtime,
            )?;
            let cancel = context.query.source.relational.runtime.cancellation_token();
            // Evaluate, validate, and stage every VALUES row before writer promotion. A scalar subquery inside VALUES may carry FOR UPDATE, so holding the backend writer during that wait would fabricate a deadlock. Ordinary subqueries retain the statement snapshot while VOLATILE functions read the logical overlay left by preceding rows, matching PostgreSQL 18 command visibility.
            let snapshot_scope = scope.returning_statement_snapshot_scope();
            let overlay = MutationOverlayScope::new(mutation.state);
            let mut conflict_locks = InsertConflictLocks::new(&preparation.referential);
            let prepared_schema = prepared_insert_spill_schema();
            let mut prepared_rows = crate::SpillBuffer::new(
                crate::query::projection::physical_work_mem_bytes(
                    context.query.source.relational.runtime,
                )?
                .max(1),
            );
            let mut supplied_identities = SuppliedIdentities::default();
            let mut events = crate::mutation::events::MutationEventQueue::default();
            let mut has_prepared_effect = false;
            let mut has_prepared_auto_identity = false;
            let discarded_identities =
                super::identity_targets::user_value_identity_columns_with_control(
                    mutation.identities,
                    stmt,
                    columns.iter().map(|column| column.column.as_str()),
                    &control,
                )?;
            for row in &stmt.rows {
                cancel.check()?;
                if row.len() > columns.len() || (!implicit_columns && row.len() != columns.len()) {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: if row.len() > columns.len() {
                            "INSERT has more expressions than target columns"
                        } else {
                            "INSERT has more target columns than expressions"
                        }
                        .into(),
                    });
                }
                let mut document = Document::new();
                for (i, col) in columns.iter().take(row.len()).enumerate() {
                    if discarded_identities.contains(&col.column) {
                        continue;
                    }
                    let value = eval_mutation_assignment(
                        read_assignment,
                        &snapshot_scope,
                        MutationAssignmentTarget {
                            table: &stmt.table,
                            target: col,
                            current: document.get(&col.column),
                            final_column_write: !columns[i + 1..]
                                .iter()
                                .any(|next| next.column == col.column),
                            action: "INSERT",
                            new_row: true,
                        },
                        &row[i],
                        None,
                        params,
                    )?;
                    if let Some(value) = value {
                        document.insert(col.column.clone(), value);
                    }
                }
                apply_missing_column_defaults(assignment, &stmt.table, &mut document, params)?;
                let prepared_auto_identity = prepare_auto_increment_identity(
                    mutation.identities,
                    &stmt.table,
                    &id_column,
                    identity_source,
                    auto_id_col.as_deref(),
                    &mut document,
                    "prepare INSERT identity",
                )?;
                has_prepared_auto_identity |= prepared_auto_identity.is_some();
                let target_table = partition_insert_target(
                    preparation.referential.constraints,
                    statement,
                    &stmt.table,
                    &document,
                    params,
                    stmt.include_descendants,
                )?;
                preparation.referential.locking.session.lock_relation(
                    &target_table,
                    crate::row_locks::RelationLockMode::RowExclusive,
                )?;
                let insert_identity = match prepared_auto_identity {
                    Some(identity) => identity,
                    None => prepare_insert_identity(
                        mutation.identities,
                        &target_table,
                        &id_column,
                        identity_source,
                        None,
                        &mut document,
                        "prepare INSERT identity",
                    )?,
                };
                if let Some(staged) = prepare_values_insert_row(
                    preparation,
                    stmt,
                    statement,
                    params,
                    &snapshot_scope,
                    conflict_update_columns.unwrap_or(&[]),
                    &id_column,
                    identity_source,
                    target_table,
                    document,
                    insert_identity,
                    &mut conflict_locks,
                )? {
                    if let Some(returning) = staged.returning {
                        returning_rows.push_to(
                            returning,
                            context.mutation.preparation.returning.destination,
                        )?;
                    }
                    events.append_after_rows(staged.after_row_events, &triggers)?;
                    if staged.prepared_effect {
                        affected += 1;
                        has_prepared_effect = true;
                    }
                    supplied_identities.note(&staged.target_table, &staged.prepared);
                    push_prepared_insert_spill_row(
                        &mut prepared_rows,
                        &prepared_schema,
                        PreparedInsertSpillRow {
                            target_table: staged.target_table,
                            document: Arc::unwrap_or_clone(staged.document),
                            conflict: staged.prepared,
                        },
                    )?;
                }
            }
            let published = overlay.finish();
            if has_prepared_effect || has_prepared_auto_identity {
                mutation.state.prepare_writer()?;
                persist_auto_increment_identity(
                    mutation.identities,
                    &stmt.table,
                    auto_id_col.as_deref(),
                    "persist INSERT identity",
                )?;
            }
            drop(conflict_locks);
            let observed = supplied_identities.observe(mutation.publication.storage)?;
            let mut publication =
                statement_end::publication_batch(&statement_commands).with_published(published);
            let mut known_new = KnownNewInserts::new(
                preparation.referential.constraints.catalog,
                &id_column,
                stmt.on_conflict.is_some(),
            );
            for row in prepared_rows
                .drain_rows()
                .map_err(crate::physical::physical_exec_error)?
            {
                cancel.check()?;
                let PreparedInsertSpillRow {
                    target_table,
                    document,
                    conflict: prepared,
                } = decode_prepared_insert_spill_row(
                    row.map_err(crate::physical::physical_exec_error)?,
                )?;
                let inserted = known_new.identity(
                    &target_table,
                    &prepared,
                    &observed,
                    mutation.publication.identifiers,
                )?;
                apply_validated_prepared_insert(
                    mutation.publication,
                    &target_table,
                    document,
                    prepared,
                    inserted,
                    &mut publication,
                )?;
            }
            finish_mutation_publication(mutation.publication, &mut publication)?;
            statement_end::note_written_rows(&statement_commands, &mut publication);
            statement_end::end_command(
                &statement_commands,
                &triggers,
                &insert_statement_events(
                    &stmt.table,
                    insert_original_query,
                    conflict_update_columns,
                ),
                events.into_after_rows(),
            )?;
            if !stmt.returning.is_empty() {
                let shape = DmlReturningShape {
                    table: &stmt.table,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: scope,
                    supplemental_schema: None,
                };
                return dml_returning_result(
                    preparation.returning,
                    shape,
                    returning_rows,
                    affected,
                );
            }
            Ok(MutationOutput::from_affected(affected))
        };
    let result = match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    };
    statement_end::finish_statement(
        context,
        params,
        &stmt.ctes,
        statement_scope.as_mut(),
        result,
    )
}
