//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table UPDATE execution, with read snapshots selected after statement trigger dispatch.
use crate::mutation::output::MutationOutput;
use crate::mutation::statement::context::{with_mutation_snapshot, MutationStatementContext};
use crate::mutation::{
    candidate::PhysicalMutationLockTarget,
    command_scope::MutationOverlayScope,
    prepared::{
        decode_prepared_mutation_action_row, prepared_mutation_action_schema,
        push_prepared_mutation_action, PreparedMutationAction,
    },
    returning::DmlReturningShape,
    statement_end,
    triggers::queue::StatementEvent,
};
use crate::query::CteScope;
use std::collections::BTreeSet;
use uqa_sql::{
    plan::UpdatePlan,
    semantics::{
        mutation_qualifiers::validate_dml_expression_qualifiers,
        returning::validate_returning_alias_relations,
    },
    SQLError, SQLParam,
};
#[expect(
    clippy::too_many_lines,
    reason = "preserves statement trigger, snapshot, row mutation, and publication order"
)]
pub fn run_table_update<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let (statement_commands, _running_statement) =
        statement_end::statement_commands(inherited_ctes);
    let _trigger_scope = crate::mutation::triggers::TriggerStatementScope::enter();
    context.query.source.locking.session.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    uqa_sql::semantics::rules::validate_rule_returning_contract(
        context.mutation.rules.rules.analysis.rules,
        &stmt.table,
        uqa_sql::ast::RuleEvent::Update,
        !stmt.returning.is_empty(),
    )?;
    if let Some(view_returning) = &stmt.view_rule_returning {
        uqa_sql::semantics::rules::validate_rule_returning_contract(
            context.mutation.rules.rules.analysis.rules,
            &view_returning.relation,
            uqa_sql::ast::RuleEvent::Update,
            !view_returning.returning.is_empty(),
        )?;
    }
    if stmt.view_rule_update_plans.is_empty() {
        uqa_sql::assignment::columns::validate_mutation_targets(
            context.mutation.preparation.referential.assignment.columns,
            &stmt.table,
            stmt.assignments
                .iter()
                .flat_map(|assignment| assignment.target.targets()),
            "UPDATE",
            false,
        )?;
        uqa_sql::semantics::generated_values::GeneratedValueColumns::of(
            context.mutation.preparation.referential.assignment.columns,
            &stmt.table,
        )?
        .validate_update(stmt.assignments.iter().flat_map(|assignment| {
            assignment.target.targets().iter().map(move |target| {
                (
                    target.column.as_str(),
                    matches!(assignment.value, crate::ScalarExpr::Default),
                )
            })
        }))?;
    }
    let privilege_expressions =
        uqa_sql::semantics::mutation_privileges::ensure_update_target_privileges(
            context.mutation.privileges,
            stmt,
        )?;
    let assigned_columns = stmt
        .assignments
        .iter()
        .flat_map(|assignment| assignment.target.column_names())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let statement_relation = crate::mutation::constraints::statement_relation(
        context.mutation.preparation.referential.constraints,
        &stmt.table,
    )?;
    let statement = crate::mutation::constraints::ConstraintStatement::new(
        &statement_relation,
        &assigned_columns,
    );
    crate::mutation::routine_calls::update(context, stmt, params, inherited_ctes)?;
    let has_before_statement_trigger = !context
        .mutation
        .preparation
        .referential
        .triggers
        .catalog
        .triggers_for(
            &stmt.table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Update,
            false,
            &assigned_columns,
        )?
        .is_empty();
    let statement_snapshot = match inherited_ctes.and_then(CteScope::command_cte_snapshot) {
        Some(snapshot) => Some(snapshot),
        None if has_before_statement_trigger
            || stmt.ctes.iter().any(|cte| cte.body.modifies_data()) =>
        {
            Some(std::sync::Arc::new(context.snapshots.capture()?))
        }
        None => None,
    };
    let update_statement = StatementEvent::new(
        &stmt.table,
        uqa_sql::ast::TriggerEvent::Update,
        &assigned_columns,
    );

    statement_commands.after_triggers().fire_before_statement(
        &context.mutation.preparation.referential.triggers,
        &update_statement,
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
                || {
                    uqa_sql::semantics::primary_command_cte_references(
                        &stmt.ctes,
                        stmt.query_inputs(),
                        stmt.source_input(),
                    )
                },
                params,
                ctes,
            )?;
            ctes.scalar_subqueries = stmt.subqueries.as_slice().into();

            if let Some(source) = stmt.source.as_deref() {
                crate::query::privileges::ensure_select_privileges_for_source_expressions(
                    source,
                    &privilege_expressions,
                    ctes,
                )?;
            }

            if stmt.source.is_none() {
                let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
                if let Some(predicate) = stmt.predicate.as_ref() {
                    validate_dml_expression_qualifiers(predicate, &allowed)?;
                }
                for assignment in &stmt.assignments {
                    for expression in assignment.expressions() {
                        validate_dml_expression_qualifiers(expression, &allowed)?;
                    }
                }
            }

            // UPDATE ... FROM other [WHERE ...]: build the joined relation,
            // evaluate WHERE against each joined row, and apply assignments to the
            // matching target rows.
            if let Some(source) = stmt.source.as_deref() {
                return super::from::run_update_from(
                    context,
                    read_context,
                    stmt,
                    source,
                    params,
                    ctes,
                    &statement_commands,
                );
            }
            let target_tables = read_context
                .mutation
                .preparation
                .referential
                .constraints
                .catalog
                .hierarchy_scan_tables(&stmt.table, stmt.include_descendants)?;
            let target_hierarchy = read_context
                .mutation
                .preparation
                .referential
                .constraints
                .partitions
                .catalog
                .try_table_hierarchy(&stmt.table)
                .map_err(|error| SQLError::Internal(format!("read UPDATE hierarchy: {error}")))?;
            let target_is_partitioned = target_hierarchy.partition_spec.is_some()
                || target_hierarchy.partition_bound.is_some();
            let has_runtime_scope = !ctes.rows.is_empty() || !ctes.scalar_subqueries.is_empty();
            if !has_runtime_scope
                && target_tables.len() == 1
                && !target_is_partitioned
                && statement_snapshot.is_none()
                && !context
                    .mutation
                    .preparation
                    .referential
                    .triggers
                    .catalog
                    .has_row_triggers(&stmt.table, uqa_sql::ast::TriggerEvent::Update)?
                && !crate::mutation::triggers::transition_capture_required(
                    &context.mutation.preparation.referential.triggers,
                    &stmt.table,
                    uqa_sql::ast::TriggerEvent::Update,
                    &assigned_columns,
                )?
            {
                if let Some(result) = crate::mutation::point_update::try_run_point_update(
                    context.mutation.point_update(),
                    stmt,
                    params,
                )? {
                    statement_end::end_command(
                        &statement_commands,
                        &context.mutation.preparation.referential.triggers,
                        std::slice::from_ref(&update_statement),
                        crate::mutation::triggers::events::AfterRowEvents::default(),
                    )?;
                    return Ok(result);
                }
            }
            let mut affected = 0u64;
            let mut returning_rows = crate::mutation::returning::ReturningRows::new(
                context.query.source.relational.runtime,
            )?;
            let cancel = context.query.source.relational.runtime.cancellation_token();
            // A non-volatile predicate can still use the accelerated candidate set. A VOLATILE predicate must stay in the row loop because PostgreSQL exposes each preceding logical rewrite before qualifying the next candidate.
            let predicate_is_volatile = stmt.predicate.as_ref().is_some_and(|predicate| {
                uqa_sql::semantics::volatility::expr_contains_volatile_function(
                    context.query.source.volatility,
                    predicate,
                )
            });
            let preselected =
                !has_runtime_scope && stmt.predicate.is_some() && !predicate_is_volatile;
            // Retrieval leaves must retain the command's input even if an earlier callback changes a later candidate. Scalar callbacks still run against the ordinary expression scope.
            let retrieval_snapshot = stmt
                .predicate
                .as_ref()
                .filter(|predicate| uqa_sql::semantics::contains_retrieval(predicate))
                .map(|_| {
                    read_context
                        .query
                        .generation
                        .cloned()
                        .map_or_else(|| read_context.snapshots.capture(), Ok)
                })
                .transpose()?;
            let qualification =
                crate::mutation::qualification::RowQualification::new(retrieval_snapshot);
            let mut candidates = crate::mutation::candidate_ids::CandidateIds::new(
                context.mutation.rules.rules.memory.work_mem_bytes()?,
                cancel.clone(),
            );
            for (table_index, table) in target_tables.iter().enumerate() {
                if preselected {
                    let filter = stmt.predicate.as_ref().ok_or_else(|| {
                        SQLError::Internal("UPDATE preselection is missing its predicate".into())
                    })?;
                    crate::query::block::where_filter::visit_where_doc_ids(
                        &read_context.query.source,
                        table,
                        &stmt.target_qualifier,
                        filter,
                        params,
                        ctes,
                        &mut |doc_id| candidates.push(table_index, doc_id),
                    )?;
                } else {
                    let tables = read_context.query.source.scans.tables;
                    let source = tables.table(table)?;
                    let changes = tables.command_overlay_changes(table)?.unwrap_or_default();
                    candidates.capture_table(
                        table_index,
                        source.read_documents().as_ref(),
                        &changes,
                    )?;
                }
            }
            let candidates = candidates.finish()?;
            let snapshot_ctes = ctes.returning_statement_snapshot_scope();
            let overlay = MutationOverlayScope::new(context.mutation.state);
            let action_schema = prepared_mutation_action_schema();
            let mut prepared_updates =
                crate::SpillBuffer::new(context.mutation.rules.rules.memory.work_mem_bytes()?);
            let mut events = crate::mutation::events::MutationEventQueue::default();
            let mut completed_ids = super::locked_rows::LockedRows::new(
                context.mutation.rules.rules.memory.work_mem_bytes()?,
                cancel.clone(),
                "UPDATE locked rows",
            );
            for candidate_id in candidates.read_rows()? {
                let (table_index, doc_id) = candidate_id?;
                let storage_table = target_tables
                    .get(table_index)
                    .ok_or_else(|| {
                        SQLError::Internal("mutation candidate has an invalid table ordinal".into())
                    })?
                    .clone();
                cancel.check()?;
                let Some(candidate) = read_context
                    .mutation
                    .preparation
                    .referential
                    .constraints
                    .reads
                    .raw_document(&storage_table, doc_id)?
                else {
                    continue;
                };
                let candidate_row = crate::mutation::rows::target_row_for_storage(
                    read_context
                        .mutation
                        .preparation
                        .referential
                        .assignment
                        .rows,
                    &stmt.table,
                    &storage_table,
                    &stmt.target_qualifier,
                    doc_id,
                    &candidate,
                )?;
                if let Some(filter) = stmt.predicate.as_ref() {
                    if !qualification.evaluate(
                        read_context,
                        &snapshot_ctes,
                        filter,
                        &candidate_row,
                        (&storage_table, doc_id),
                        params,
                    )? {
                        continue;
                    }
                }
                let projected = super::projection::project_update_row(
                    read_context.mutation.preparation.referential.assignment,
                    stmt,
                    params,
                    &snapshot_ctes,
                    candidate.clone(),
                    &candidate_row,
                )?;
                let target = crate::mutation::locking::lock_physical_mutation_target(
                    context.mutation.preparation.referential.locking.session,
                    &storage_table,
                    &stmt.target_qualifier,
                    doc_id,
                    crate::query::locking::context::update_lock_strength(
                        context.mutation.preparation.referential.locking.catalog,
                        &storage_table,
                        &assigned_columns,
                    ),
                )?;
                let PhysicalMutationLockTarget::Present { identity, recheck } = target else {
                    continue;
                };
                // Another command of the statement already wrote the row, which `PostgreSQL`'s `ExecUpdate` skips as `TM_SelfModified`.
                if ctes.statement_wrote(&identity) {
                    continue;
                }
                // A statement that the command's triggers or functions started wrote the row under a later command id.
                if ctes.statement_triggered_write(&identity) {
                    return Err(crate::mutation::errors::triggered_modification_error(
                        "updated",
                    ));
                }
                let storage_table = identity.table;
                let doc_id = identity.doc_id;
                if completed_ids.contains(&storage_table, doc_id)? {
                    continue;
                }
                if recheck {
                    context
                        .mutation
                        .preparation
                        .referential
                        .constraints
                        .transactions
                        .refresh_explicit_statement_snapshot()?;
                }
                let Some(original_doc) = context
                    .mutation
                    .preparation
                    .referential
                    .locking
                    .rows
                    .get_document_for_mutation(&storage_table, doc_id)?
                else {
                    continue;
                };
                let doc = if recheck {
                    let target_row = crate::mutation::rows::target_row_for_storage(
                        context.mutation.preparation.referential.assignment.rows,
                        &stmt.table,
                        &storage_table,
                        &stmt.target_qualifier,
                        doc_id,
                        &original_doc,
                    )?;
                    if let Some(filter) = stmt.predicate.as_ref() {
                        let refreshed = MutationStatementContext {
                            query: context.query,
                            mutation: read_context.mutation,
                            snapshots: read_context.snapshots,
                        };
                        if !crate::mutation::qualification::RowQualification::new(None).evaluate(
                            &refreshed,
                            &snapshot_ctes,
                            filter,
                            &target_row,
                            (&storage_table, doc_id),
                            params,
                        )? {
                            continue;
                        }
                    }
                    super::projection::project_update_row(
                        read_context.mutation.preparation.referential.assignment,
                        stmt,
                        params,
                        &snapshot_ctes,
                        original_doc.clone(),
                        &target_row,
                    )?
                } else {
                    projected
                };

                if let Some(prepared) = crate::mutation::update::prepare_update_row(
                    context.mutation.preparation,
                    stmt,
                    params,
                    &snapshot_ctes,
                    statement,
                    &storage_table,
                    doc_id,
                    original_doc,
                    doc,
                    None,
                )? {
                    completed_ids.insert(&storage_table, doc_id)?;
                    if let Some(returning) = prepared.returning {
                        returning_rows.push_to(
                            returning,
                            context.mutation.preparation.returning.destination,
                        )?;
                    }
                    affected += u64::from(prepared.affected);
                    push_prepared_mutation_action(
                        &mut prepared_updates,
                        &action_schema,
                        PreparedMutationAction::Rewrite(prepared.rewrite),
                    )?;
                    events.append_after_rows(
                        prepared.after_row_events,
                        &context.mutation.preparation.referential.triggers,
                    )?;
                }
            }
            let published = overlay.finish();
            if prepared_updates.rows() != 0 {
                context.mutation.state.prepare_writer()?;
                let mut publication =
                    statement_end::publication_batch(&statement_commands).with_published(published);
                for row in prepared_updates
                    .drain_rows()
                    .map_err(crate::physical::physical_exec_error)?
                {
                    cancel.check()?;
                    let action = decode_prepared_mutation_action_row(
                        row.map_err(crate::physical::physical_exec_error)?,
                    )?;
                    crate::mutation::publication::publish_prepared_mutation_action(
                        context.mutation.publication,
                        action,
                        crate::mutation::publication::InsertedIdentity::Unknown,
                        &mut publication,
                    )?;
                }
                crate::mutation::publication::finish_mutation_publication(
                    context.mutation.publication,
                    &mut publication,
                )?;
                statement_end::note_written_rows(&statement_commands, &mut publication);
            }
            statement_end::end_command(
                &statement_commands,
                &context.mutation.preparation.referential.triggers,
                std::slice::from_ref(&update_statement),
                events.into_after_rows(),
            )?;
            if !stmt.returning.is_empty() {
                let shape = DmlReturningShape {
                    table: &stmt.table,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes,
                    supplemental_schema: None,
                };

                return crate::mutation::returning::dml_returning_result(
                    context.mutation.preparation.returning,
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
