//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table INSERT scheduling, snapshot reads, row staging, and publication.
use super::{
    codec::{decode_prepared_insert_spill_row, PreparedInsertSpillRow},
    known_new::KnownNewInserts,
    rows::prepare_values_insert_row,
    source::{
        insert_source_expression_rows, InsertSelectConsumer, InsertSelectIdentity,
        PreparedInsertSelect,
    },
    supplied_identities::SuppliedIdentities,
    triggers::insert_statement_events,
};
use crate::mutation::statement::context::{with_mutation_snapshot, MutationStatementContext};
use crate::{
    mutation::{
        assignment::{
            apply_missing_column_defaults, eval_mutation_assignment, MutationAssignmentTarget,
        },
        command_scope::MutationOverlayScope,
        conflict::update::InsertConflictLocks,
        constraints::{partition_insert_target, ConstraintStatement},
        errors::dml_storage_error,
        identity::{
            insert_identity_columns, persist_auto_increment_identity,
            prepare_auto_increment_identity, prepare_insert_identity,
        },
        publication::{apply_validated_prepared_insert, finish_mutation_publication},
        returning::{dml_returning_result, DmlReturningShape},
        statement_end,
        triggers::queue::StatementEvent,
    },
    query::{statement::consumer::QueryOutputMode, CteScope},
};
use std::{collections::BTreeSet, rc::Rc, sync::Arc};
use uqa_sql::assignment::columns::validate_mutation_targets;
use uqa_sql::{
    plan::{ConflictActionPlan, ConflictPlan, InsertPlan, QueryPlan},
    semantics::{
        conflict::InferenceContext,
        returning::{validate_returning_alias_relations, ReturningAnalysisContext},
        rules::insert_inputs::{
            required_view_rule_insert_input_positions, view_rule_insert_column_type,
        },
    },
    SQLError, SQLParam, SQLResult,
};
use uqa_storage::document_store::Document;

/// SQL analysis inputs and the planner's positional source-output rewrite for one INSERT.
pub struct InsertPlanning<'a> {
    pub inference: InferenceContext<'a>,
    pub returning: ReturningAnalysisContext<'a>,
    pub prune_source_outputs: fn(&mut QueryPlan, &BTreeSet<usize>, usize),
}

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub fn run_table_insert<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    planning: InsertPlanning<'_>,
    stmt: &InsertPlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    let mutation = &context.mutation;
    let preparation = mutation.preparation;
    let assignment = preparation.referential.assignment;
    let triggers = preparation.referential.triggers;
    let statement_commands = statement_end::statement_commands(inherited_ctes);
    let _trigger_scope = crate::mutation::triggers::TriggerStatementScope::enter();
    preparation.referential.locking.session.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    let insert_rules = mutation
        .rules
        .rules
        .analysis
        .rules
        .rules_for(&stmt.table, uqa_sql::ast::RuleEvent::Insert)?;
    let has_insert_rules = !insert_rules.is_empty();
    let has_view_insert_rules = !stmt.view_rule_relations.is_empty();
    let has_any_insert_rules = has_insert_rules || has_view_insert_rules;
    if stmt.on_conflict.is_some()
        && (has_insert_rules
            || !mutation
                .rules
                .rules
                .analysis
                .rules
                .rules_for(&stmt.table, uqa_sql::ast::RuleEvent::Update)?
                .is_empty())
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
    let prepared_statement = uqa_sql::semantics::conflict::prepare_inference_predicate(
        planning.inference,
        stmt,
        params,
    )?;
    let stmt = prepared_statement.as_ref();
    if let Some(conflict) = stmt.on_conflict.as_ref() {
        uqa_sql::semantics::conflict::validate_conflict_target(
            planning.inference.catalog,
            &stmt.table,
            conflict,
        )?;
    }
    let conflict_update_columns = if let Some(ConflictPlan {
        action: ConflictActionPlan::Update { assignments, .. },
        ..
    }) = stmt.on_conflict.as_ref()
    {
        validate_mutation_targets(
            assignment.columns,
            &stmt.table,
            assignments.iter().map(|assignment| &assignment.target),
            "INSERT ON CONFLICT DO UPDATE",
            false,
        )?;
        Some(
            assignments
                .iter()
                .map(|assignment| assignment.target.column.clone())
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    if stmt.view_rule_relations.is_empty() && !stmt.columns.is_empty() {
        validate_mutation_targets(
            assignment.columns,
            &stmt.table,
            stmt.columns.iter(),
            "INSERT",
            true,
        )?;
    }
    if stmt.view_rule_relations.is_empty() {
        super::identity_targets::validate_insert_identity_targets(mutation.identities, stmt)?;
    }
    uqa_sql::semantics::returning::validate_insert_returning(
        planning.returning,
        stmt,
        params,
        inherited_ctes.map(|scope| scope as &dyn uqa_sql::semantics::returning::ReturningScope),
    )?;
    uqa_sql::semantics::mutation_privileges::ensure_insert_target_privileges(
        mutation.privileges,
        stmt,
        conflict_update_columns.as_deref(),
    )?;
    // The columns the statement supplies, which its constraint violations show to a role that may not read the table.
    let supplied_columns =
        uqa_sql::semantics::mutation_privileges::insert_target_columns(mutation.privileges, stmt)?
            .into_iter()
            .chain(conflict_update_columns.iter().flatten().cloned())
            .collect::<Vec<_>>();
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
                    .analysis
                    .rules
                    .rules_for(relation, uqa_sql::ast::RuleEvent::Insert)?
                    .iter()
                    .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
        },
    )?;
    let insert_original_query = view_original_query
        && !insert_rules
            .iter()
            .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
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
    let has_before_update_statement_trigger =
        if let Some(columns) = conflict_update_columns.as_deref() {
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
    if let Some(columns) = conflict_update_columns.as_deref() {
        statement_commands.after_triggers().fire_before_statement(
            &triggers,
            &StatementEvent::new(&stmt.table, uqa_sql::ast::TriggerEvent::Update, columns),
        )?;
    }
    let mut statement_scope = None;
    let mut execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<SQLResult, SQLError> {
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
                || {
                    uqa_sql::semantics::primary_command_cte_references(
                        &stmt.ctes,
                        &stmt.query_inputs(),
                        None,
                    )
                },
                params,
                scope,
            )?;
            scope.scalar_subqueries.clone_from(&stmt.subqueries);
            // Resolve the column that names an inserted row: the table's single PRIMARY KEY column, whether a sequence generates its values or not, and otherwise the conventional legacy `id` slot of a table without declared columns. Both VALUES and SELECT sources must derive the internal doc id from this same column or later primary-key rewrites can address a different row than the one that was inserted.
            let (auto_id_col, id_column, identity_source) =
                insert_identity_columns(mutation.identities, &stmt.table, "INSERT")?;
            let mut rule_source_rows = None;
            // INSERT ... SELECT: the query executor feeds each positional physical row directly into the INSERT sink. Ordinary source scans and scalar subqueries retain the statement snapshot, while a VOLATILE callback observes the logical mutations staged by preceding rows of this command.
            if let Some(source) = stmt.source.as_deref() {
                let surviving_view_rules_require_rows =
                    uqa_sql::semantics::rules::analysis::surviving_view_rules_require_event_rows(
                        mutation.rules.rules.analysis,
                        &stmt.view_rule_relations,
                        uqa_sql::ast::RuleEvent::Insert,
                    )?;
                if !view_original_query && !surviving_view_rules_require_rows {
                    rule_source_rows = Some(Vec::new());
                } else if !has_any_insert_rules {
                    let snapshot_scope = scope.returning_statement_snapshot_scope();
                    let mut source_scope = snapshot_scope.clone();
                    source_scope.enable_command_progress_streaming();
                    let consumer = Rc::new(InsertSelectConsumer::new(
                        context.insert_source(),
                        stmt,
                        params,
                        snapshot_scope,
                        InsertSelectIdentity {
                            auto_id_column: auto_id_col.clone(),
                            id_column: id_column.clone(),
                            identity_source,
                        },
                        conflict_update_columns.clone().unwrap_or_default(),
                    )?);
                    let overlay = MutationOverlayScope::new(mutation.state);
                    crate::query::statement::execute_query_plan_output(
                        &read_context.query,
                        source,
                        params,
                        &mut source_scope,
                        QueryOutputMode::RowConsumer(Rc::new(
                            super::source::binding::InsertSelectOutput {
                                binding: mutation.insert_consumers,
                                consumer: Rc::clone(&consumer),
                            },
                        )),
                    )?;
                    let PreparedInsertSelect {
                        rows: prepared_rows,
                        conflict_locks,
                        affected,
                        returning_rows,
                        events,
                        has_prepared_effect,
                        has_prepared_auto_identity,
                        supplied_identities,
                    } = consumer.take_prepared()?;
                    drop(overlay);
                    if has_prepared_effect || has_prepared_auto_identity {
                        mutation.state.prepare_writer()?;
                        persist_auto_increment_identity(
                            mutation.identities,
                            &stmt.table,
                            auto_id_col.as_deref(),
                            "persist INSERT SELECT identity",
                        )?;
                    }
                    drop(conflict_locks);
                    let observed = supplied_identities.observe(mutation.publication.storage)?;
                    let cancel = context.query.source.relational.runtime.cancellation_token();
                    let apply_reader = prepared_rows
                        .read_rows()
                        .map_err(crate::physical::physical_exec_error)?;
                    let mut publication = statement_end::publication_batch(&statement_commands);
                    let mut known_new = KnownNewInserts::new(
                        preparation.referential.constraints.catalog,
                        &id_column,
                        stmt.on_conflict.is_some(),
                    );
                    for prepared_row in apply_reader {
                        cancel.check()?;
                        let prepared_row =
                            prepared_row.map_err(crate::physical::physical_exec_error)?;
                        let PreparedInsertSpillRow {
                            target_table,
                            document,
                            conflict: prepared,
                        } = decode_prepared_insert_spill_row(prepared_row)?;
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
                            conflict_update_columns.as_deref(),
                        ),
                        events.into_after_rows(),
                    )?;
                    if !stmt.returning.is_empty() {
                        return dml_returning_result(
                            preparation.returning,
                            DmlReturningShape {
                                table: &stmt.table,
                                target_qualifier: &stmt.target_qualifier,
                                aliases: &stmt.returning_aliases,
                                returning: &stmt.returning,
                                params,
                                ctes: scope,
                                supplemental_schema: None,
                            },
                            returning_rows,
                            affected,
                        );
                    }
                    return Ok(SQLResult::from_affected(affected));
                }
                if rule_source_rows.is_none() {
                    let mut source = source.clone();
                    if !view_original_query {
                        if let Some(required_positions) = required_view_rule_insert_input_positions(
                            mutation.rules.rules.analysis,
                            stmt,
                        )? {
                            (planning.prune_source_outputs)(
                                &mut source,
                                &required_positions,
                                stmt.columns.len(),
                            );
                        }
                    }
                    let mut source_scope = scope.returning_statement_snapshot_scope();
                    source_scope.enable_command_progress_streaming();
                    let result = crate::query::statement::execute_query_plan_with_ctes(
                        &read_context.query,
                        &source,
                        params,
                        &mut source_scope,
                    )?;
                    rule_source_rows = Some(insert_source_expression_rows(result)?);
                }
            }

            let implicit_columns = stmt.columns.is_empty();
            let columns: Vec<uqa_sql::ast::AssignmentTarget<uqa_sql::ScalarExpr>> =
                if implicit_columns {
                    // INSERT without explicit column list: project the table schema.
                    preparation
                        .returning
                        .catalog
                        .try_table_columns(&stmt.table)
                        .map_err(|error| dml_storage_error("INSERT", error))?
                        .into_iter()
                        .map(Into::into)
                        .collect()
                } else {
                    stmt.columns.clone()
                };
            if view_original_query {
                validate_mutation_targets(
                    assignment.columns,
                    &stmt.table,
                    columns.iter(),
                    "INSERT",
                    true,
                )?;
            }

            // No explicit id and no auto-increment column: allocate a synthetic u64 doc_id at insert time. Every table has an implicit doc_id even when the schema declares no primary key.

            let mut affected = 0u64;
            let mut returning_rows = Vec::new();
            let cancel = context.query.source.relational.runtime.cancellation_token();
            // Evaluate, validate, and stage every VALUES row before writer promotion. A scalar subquery inside VALUES may carry FOR UPDATE, so holding the backend writer during that wait would fabricate a deadlock. Ordinary subqueries retain the statement snapshot while VOLATILE functions read the logical overlay left by preceding rows, matching PostgreSQL 18 command visibility.
            let snapshot_scope = scope.returning_statement_snapshot_scope();
            let overlay = MutationOverlayScope::new(mutation.state);
            let mut conflict_locks = InsertConflictLocks::new(&preparation.referential);
            let input_rows = rule_source_rows.as_deref().unwrap_or(&stmt.rows);
            let mut documents = Vec::with_capacity(input_rows.len());
            let mut target_tables = Vec::with_capacity(input_rows.len());
            let mut prepared_conflicts = Vec::with_capacity(input_rows.len());
            let mut events = crate::mutation::events::MutationEventQueue::default();
            let mut has_prepared_effect = false;
            let mut has_prepared_auto_identity = false;
            let mut pending_rule_rows = Vec::with_capacity(input_rows.len());
            let discarded_identities = super::identity_targets::user_value_identity_columns(
                mutation.identities,
                stmt,
                columns.iter().map(|column| column.column.as_str()),
            )?;
            let required_rule_input_positions = (!view_original_query)
                .then(|| {
                    required_view_rule_insert_input_positions(mutation.rules.rules.analysis, stmt)
                })
                .transpose()?
                .flatten();
            for row in input_rows {
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
                    if required_rule_input_positions
                        .as_ref()
                        .is_some_and(|required| !required.contains(&i))
                    {
                        continue;
                    }
                    let value = if has_any_insert_rules
                        && matches!(row[i], uqa_sql::ScalarExpr::Default)
                    {
                        None
                    } else if !view_original_query {
                        let ty =
                            view_rule_insert_column_type(mutation.rules.views.rewrite, stmt, i)?;
                        Some(crate::mutation::assignment::eval_typed_assignment(
                            read_assignment,
                            &snapshot_scope,
                            crate::mutation::assignment::TypedAssignmentTarget {
                                target: col,
                                ty: ty.as_ref(),
                                current: document.get(&col.column),
                                final_column_write: !columns[i + 1..]
                                    .iter()
                                    .any(|next| next.column == col.column),
                            },
                            &row[i],
                            None,
                            params,
                        )?)
                    } else {
                        eval_mutation_assignment(
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
                        )?
                    };
                    if let Some(value) = value {
                        document.insert(col.column.clone(), value);
                    }
                }
                if has_any_insert_rules {
                    pending_rule_rows.push(document);
                    continue;
                }
                apply_missing_column_defaults(assignment, &stmt.table, &mut document, params)?;
                let prepared_auto_identity = prepare_auto_increment_identity(
                    mutation.identities,
                    &stmt.table,
                    &id_column,
                    identity_source,
                    auto_id_col.as_deref(),
                    stmt.overriding,
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
                    conflict_update_columns.as_deref().unwrap_or(&[]),
                    &id_column,
                    identity_source,
                    target_table,
                    document,
                    insert_identity,
                    &mut conflict_locks,
                )? {
                    if let Some(returning) = staged.returning {
                        returning_rows.push(returning);
                    }
                    events.append_after_rows(staged.after_row_events);
                    if staged.prepared_effect {
                        affected += 1;
                        has_prepared_effect = true;
                    }
                    documents.push(staged.document);
                    target_tables.push(staged.target_table);
                    prepared_conflicts.push(staged.prepared);
                }
            }
            let mut view_rule_rows = Vec::with_capacity(pending_rule_rows.len());
            for document in &pending_rule_rows {
                let rule_doc_id = crate::mutation::identity::supplied_document_identity(
                    identity_source,
                    document,
                    &id_column,
                )?;
                view_rule_rows.push(crate::mutation::rules::RuleRowImage {
                    old_storage_table: None,
                    old_doc_id: None,
                    old: None,
                    new_storage_table: None,
                    new_doc_id: rule_doc_id,
                    new: Some(document.clone()),
                    context: None,
                });
            }
            let view_rule_batches = crate::mutation::rules::views::prepare_view_rule_batches(
                crate::mutation::rules::views::ViewRuleBatchRequest {
                    context: mutation.rules,
                    relations: &stmt.view_rule_relations,
                    event: uqa_sql::ast::RuleEvent::Insert,
                    rows: &view_rule_rows,
                    params,
                    scope: &snapshot_scope,
                    insert_plans: &stmt.view_rule_insert_plans,
                    update_plans: &[],
                    document_relation: None,
                },
            )?;
            let mut pending_base_rows = Vec::with_capacity(pending_rule_rows.len());
            for (index, mut document) in pending_rule_rows.into_iter().enumerate() {
                if view_rule_batches.suppresses(index) {
                    continue;
                }
                apply_missing_column_defaults(assignment, &stmt.table, &mut document, params)?;
                let prepared_auto_identity = prepare_auto_increment_identity(
                    mutation.identities,
                    &stmt.table,
                    &id_column,
                    identity_source,
                    auto_id_col.as_deref(),
                    stmt.overriding,
                    &mut document,
                    "prepare INSERT identity",
                )?;
                has_prepared_auto_identity |= prepared_auto_identity.is_some();
                pending_base_rows.push((document, prepared_auto_identity));
            }
            let rule_batch = (has_insert_rules && view_original_query)
                .then(|| {
                    let rule_rows = pending_base_rows
                        .iter()
                        .map(|(document, _)| {
                            let mut rule_document = document.clone();
                            crate::mutation::assignment::refresh_stored_generated_columns(
                                assignment,
                                &stmt.table,
                                &mut rule_document,
                            )?;
                            let rule_doc_id =
                                crate::mutation::identity::supplied_document_identity(
                                    identity_source,
                                    &rule_document,
                                    &id_column,
                                )?;
                            Ok(crate::mutation::rules::RuleRowImage {
                                old_storage_table: None,
                                old_doc_id: None,
                                old: None,
                                new_storage_table: None,
                                new_doc_id: rule_doc_id,
                                new: Some(rule_document),
                                context: None,
                            })
                        })
                        .collect::<Result<Vec<_>, SQLError>>()?;
                    crate::mutation::rules::prepare_rule_batch(
                        mutation.rules.rules,
                        &stmt.table,
                        uqa_sql::ast::RuleEvent::Insert,
                        rule_rows,
                    )
                })
                .transpose()?;
            if has_any_insert_rules {
                for (rule_index, (mut document, prepared_auto_identity)) in
                    pending_base_rows.into_iter().enumerate()
                {
                    if rule_batch
                        .as_ref()
                        .is_some_and(|rule_batch| rule_batch.suppresses(rule_index))
                    {
                        continue;
                    }
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
                    let Some(staged) = prepare_values_insert_row(
                        preparation,
                        stmt,
                        statement,
                        params,
                        &snapshot_scope,
                        conflict_update_columns.as_deref().unwrap_or(&[]),
                        &id_column,
                        identity_source,
                        target_table,
                        document,
                        insert_identity,
                        &mut conflict_locks,
                    )?
                    else {
                        continue;
                    };
                    if let Some(returning) = staged.returning {
                        returning_rows.push(returning);
                    }
                    events.append_after_rows(staged.after_row_events);
                    if staged.prepared_effect {
                        affected += 1;
                        has_prepared_effect = true;
                    }
                    documents.push(staged.document);
                    target_tables.push(staged.target_table);
                    prepared_conflicts.push(staged.prepared);
                }
            }
            drop(overlay);
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
            let mut supplied_identities = SuppliedIdentities::default();
            for (target_table, prepared) in target_tables.iter().zip(&prepared_conflicts) {
                supplied_identities.note(target_table, prepared);
            }
            let observed = supplied_identities.observe(mutation.publication.storage)?;
            let mut publication = statement_end::publication_batch(&statement_commands);
            let mut known_new = KnownNewInserts::new(
                preparation.referential.constraints.catalog,
                &id_column,
                stmt.on_conflict.is_some(),
            );
            for ((target_table, document), prepared) in target_tables
                .into_iter()
                .zip(documents)
                .zip(prepared_conflicts)
            {
                cancel.check()?;
                let inserted = known_new.identity(
                    &target_table,
                    &prepared,
                    &observed,
                    mutation.publication.identifiers,
                )?;
                let document = Arc::try_unwrap(document).map_err(|_| {
                    SQLError::Internal("INSERT command overlay retained a staged document".into())
                })?;
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
                    conflict_update_columns.as_deref(),
                ),
                events.into_after_rows(),
            )?;
            let (rule_returning, rule_affected, rule_sets_command_tag) =
                if let Some(rule_batch) = rule_batch.as_ref() {
                    let outcome = rule_batch.execute_actions_with_affected(
                        mutation.rules.rules,
                        crate::mutation::rules::RuleReturningRequest::from_plan(
                            &stmt.returning,
                            &stmt.returning_aliases,
                            &stmt.subqueries,
                        ),
                    )?;
                    (
                        outcome.returning,
                        outcome.affected_rows,
                        outcome.sets_command_tag,
                    )
                } else {
                    (None, 0, false)
                };
            let view_rule_outcome = view_rule_batches.execute_actions_with_affected(
                mutation.rules.rules,
                stmt.view_rule_returning.as_ref(),
            )?;
            let view_rule_returning = view_rule_outcome.returning;
            if view_rule_returning.is_some() && rule_returning.is_some() {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "cannot have RETURNING lists in multiple rules".into(),
                });
            }
            if !stmt.returning.is_empty() {
                if let Some(view_rule_returning) = view_rule_returning {
                    return view_rule_returning.project(preparation.returning, params, scope, None);
                }
                let shape = DmlReturningShape {
                    table: &stmt.table,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: scope,
                    supplemental_schema: None,
                };
                if let Some(rule_returning) = rule_returning {
                    return rule_returning.project(preparation.returning, shape);
                }
                return dml_returning_result(
                    preparation.returning,
                    shape,
                    returning_rows,
                    affected,
                );
            }
            let rule_affected = if view_rule_outcome.sets_command_tag {
                view_rule_outcome.affected_rows
            } else if rule_sets_command_tag {
                rule_affected
            } else {
                0
            };
            Ok(SQLResult::from_affected(
                if affected == 0 && !insert_original_query {
                    rule_affected
                } else {
                    affected
                },
            ))
        };
    let result = match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    }?;
    statement_end::finish_statement(context, params, &stmt.ctes, statement_scope.as_mut())?;
    Ok(result)
}
