//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT conflict tuple locking, EXCLUDED projection and typed update preparation.
use super::{find_insert_conflict, CurrentInsertConflict, InsertConflictOverlay};
use crate::mutation::{
    assignment::{eval_mutation_assignment, MutationAssignmentTarget},
    candidate::{MutationLockTarget, PhysicalDocumentIdentity},
    constraints::lock_document_key_dependencies,
    errors::{dml_storage_error, missing_document_error},
    events::ReferentialActionContext,
    expressions::eval_mutation_expr,
    locking::{lock_mutation_target, MutationLockCleanup},
    prepared::PreparedInsertConflict,
    referential::{prepare_document_rewrite, reject_partition_rewrite, ReferentialContext},
    rows::{
        append_hidden_qualified_row as dml_append_hidden_qualified_row,
        target_row as dml_target_row,
    },
};
use crate::query::locking::context::update_lock_strength;
use crate::query::CteScope;
use uqa_core::{DocId, Value};
use uqa_sql::{
    plan::{ConflictActionPlan, ConflictPlan},
    SQLError, SQLParam,
};
use uqa_storage::document_store::Document;
pub struct InsertConflictPreparation<'a, S: Clone + 'static> {
    pub context: ReferentialContext<'a, S>,
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub on_conflict: &'a ConflictPlan,
    pub document: &'a Document,
    pub params: &'a [SQLParam],
    pub scope: &'a CteScope<S>,
}

fn on_conflict_cardinality_violation() -> SQLError {
    SQLError::Routine {
        sqlstate: "21000".into(),
        message: "ON CONFLICT DO UPDATE command cannot affect row a second time\nHINT: Ensure that no rows proposed for insertion within the same command have duplicate constrained values.".into(),
    }
}

enum BuiltConflictUpdate {
    Skip,
    Update {
        old_document: Document,
        new_document: Document,
    },
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
fn build_conflict_update<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    target_qualifier: &str,
    existing_id: DocId,
    document: &Document,
    assignments: &[uqa_sql::plan::AssignmentPlan],
    predicate: Option<&crate::ScalarExpr>,
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<BuiltConflictUpdate, SQLError> {
    let existing_doc = context
        .locking
        .rows
        .get_document_for_mutation(table, existing_id)?
        .ok_or_else(|| missing_document_error("INSERT ON CONFLICT", table, existing_id))?;
    let target_row = dml_target_row(
        context.assignment.rows,
        table,
        target_qualifier,
        existing_id,
        &existing_doc,
    )?;
    let definitions = context
        .constraints
        .catalog
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("INSERT EXCLUDED schema", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut excluded_document = document.clone();
    crate::query::generated::materialize_virtual_generated_columns(
        &definitions,
        &mut excluded_document,
    )?;
    let excluded_columns = if definitions.is_empty() {
        excluded_document.keys().cloned().collect::<Vec<_>>()
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let excluded_types = excluded_columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    let excluded_values = excluded_columns
        .iter()
        .map(|column| {
            excluded_document
                .get(column)
                .cloned()
                .unwrap_or(Value::Null)
        })
        .collect();
    let conflict_row = dml_append_hidden_qualified_row(
        &target_row,
        "excluded",
        &excluded_columns,
        &excluded_types,
        excluded_values,
    );
    if let Some(predicate) = predicate {
        let keep = eval_mutation_expr(
            context.assignment.expressions,
            scope,
            predicate,
            Some(&conflict_row),
            params,
        )?;
        if !uqa_sql::expr::truthy(&keep) {
            return Ok(BuiltConflictUpdate::Skip);
        }
    }
    let mut updated_doc = existing_doc.clone();
    for assignment in assignments {
        let value = eval_mutation_assignment(
            context.assignment,
            scope,
            MutationAssignmentTarget {
                table,
                column: &assignment.column,
                action: "INSERT ON CONFLICT DO UPDATE",
            },
            &assignment.value,
            Some(&conflict_row),
            params,
        )?;
        if let Some(value) = value {
            updated_doc.insert(assignment.column.clone(), value);
        } else {
            updated_doc.remove(&assignment.column);
        }
    }
    Ok(BuiltConflictUpdate::Update {
        old_document: existing_doc,
        new_document: updated_doc,
    })
}

/// Locks every currently visible ON CONFLICT dependency for an INSERT input set while the storage transaction is still a reader. DO NOTHING locks are retained only until writer promotion; DO UPDATE target locks keep their normal transaction lifetime. Once the single backend writer is held no concurrent transaction can create a new physical conflict and make the execution phase wait behind a tuple owner.
pub struct InsertConflictLocks {
    transient: MutationLockCleanup,
    overlay: Option<InsertConflictOverlay>,
}

impl InsertConflictLocks {
    pub fn new<S: Clone + 'static>(context: &ReferentialContext<'_, S>) -> Self {
        Self {
            transient: MutationLockCleanup::new(context.locking.session),
            overlay: None,
        }
    }

    pub fn lock_document<S: Clone + 'static>(
        &mut self,
        context: &ReferentialContext<'_, S>,
        table: &str,
        target_qualifier: &str,
        on_conflict: &ConflictPlan,
        document: &Document,
    ) -> Result<(), SQLError> {
        for _ in 0..=64 {
            let Some(existing) =
                find_insert_conflict(context.constraints, table, on_conflict, document)?
            else {
                return Ok(());
            };
            let (locked, recheck) = match &on_conflict.action {
                ConflictActionPlan::Nothing => (
                    existing.clone(),
                    self.transient.acquire(
                        context.locking.session,
                        &existing.table,
                        target_qualifier,
                        existing.doc_id,
                        uqa_sql::ast::LockStrength::ForKeyShare,
                    )?,
                ),
                ConflictActionPlan::Update { assignments, .. } => {
                    match lock_mutation_target(
                        context.locking.session,
                        &existing.table,
                        target_qualifier,
                        existing.doc_id,
                        update_lock_strength(
                            context.locking.catalog,
                            &existing.table,
                            &assignments
                                .iter()
                                .map(|assignment| assignment.column.clone())
                                .collect::<Vec<_>>(),
                        ),
                    )? {
                        MutationLockTarget::Present { doc_id, recheck } => (
                            PhysicalDocumentIdentity {
                                table: existing.table,
                                doc_id,
                            },
                            recheck,
                        ),
                        MutationLockTarget::Deleted => {
                            context
                                .constraints
                                .transactions
                                .refresh_explicit_statement_snapshot()?;
                            continue;
                        }
                    }
                }
            };
            if recheck {
                context
                    .constraints
                    .transactions
                    .refresh_explicit_statement_snapshot()?;
            }
            if find_insert_conflict(context.constraints, table, on_conflict, document)?
                == Some(locked)
            {
                return Ok(());
            }
        }
        Err(SQLError::Internal(format!(
            "INSERT conflict lookup for `{table}` did not converge"
        )))
    }

    #[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
    pub fn prepare_document<S: Clone + 'static>(
        &mut self,
        preparation: InsertConflictPreparation<'_, S>,
        referential_actions: &mut ReferentialActionContext,
    ) -> Result<PreparedInsertConflict, SQLError> {
        let InsertConflictPreparation {
            context,
            table,
            target_qualifier,
            on_conflict,
            document,
            params,
            scope,
        } = preparation;
        let key_acquisitions =
            lock_document_key_dependencies(context.constraints, table, document, None)?;
        if self.overlay.is_none() {
            self.overlay = Some(InsertConflictOverlay::new(
                context.constraints,
                table,
                on_conflict,
            )?);
        }
        let current = self
            .overlay
            .as_ref()
            .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
            .find(context.constraints, table, document)?;
        match current {
            None => {
                self.overlay
                    .as_mut()
                    .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
                    .note_insert(context.constraints, table, document)?;
                return Ok(PreparedInsertConflict::Unresolved);
            }
            Some(CurrentInsertConflict::Overlay) => {
                self.transient.rollback(key_acquisitions);
                return match &on_conflict.action {
                    ConflictActionPlan::Nothing => Ok(PreparedInsertConflict::Skip),
                    ConflictActionPlan::Update { .. } => Err(on_conflict_cardinality_violation()),
                };
            }
            Some(CurrentInsertConflict::Base(_)) => {}
        }
        self.lock_document(&context, table, target_qualifier, on_conflict, document)?;
        let current = self
            .overlay
            .as_ref()
            .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
            .find(context.constraints, table, document)?;
        let existing = match current {
            None => {
                self.overlay
                    .as_mut()
                    .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
                    .note_insert(context.constraints, table, document)?;
                return Ok(PreparedInsertConflict::Unresolved);
            }
            Some(CurrentInsertConflict::Overlay) => {
                self.transient.rollback(key_acquisitions);
                return match &on_conflict.action {
                    ConflictActionPlan::Nothing => Ok(PreparedInsertConflict::Skip),
                    ConflictActionPlan::Update { .. } => Err(on_conflict_cardinality_violation()),
                };
            }
            Some(CurrentInsertConflict::Base(identity)) => identity,
        };
        self.transient.retain(key_acquisitions);
        let ConflictActionPlan::Update {
            assignments,
            predicate,
        } = &on_conflict.action
        else {
            return Ok(PreparedInsertConflict::Skip);
        };
        match build_conflict_update(
            &context,
            &existing.table,
            target_qualifier,
            existing.doc_id,
            document,
            assignments,
            predicate.as_deref(),
            params,
            scope,
        )? {
            BuiltConflictUpdate::Skip => Ok(PreparedInsertConflict::Skip),
            BuiltConflictUpdate::Update {
                old_document,
                mut new_document,
            } => {
                let updated_columns = assignments
                    .iter()
                    .map(|assignment| assignment.column.clone())
                    .collect::<Vec<_>>();
                let Some(triggered_document) = crate::mutation::triggers::fire_before_row_triggers(
                    &context.triggers,
                    &existing.table,
                    uqa_sql::ast::TriggerEvent::Update,
                    existing.doc_id,
                    Some(&old_document),
                    Some(&new_document),
                    &updated_columns,
                )?
                else {
                    return Ok(PreparedInsertConflict::Skip);
                };
                new_document = triggered_document;
                let prepared = prepare_document_rewrite(
                    &context,
                    &existing.table,
                    existing.doc_id,
                    old_document,
                    new_document,
                    params,
                    referential_actions,
                )?
                .ok_or_else(|| {
                    SQLError::Internal(
                        "INSERT ON CONFLICT rewrite dependency tree was cyclic at its root".into(),
                    )
                })?;
                if let Some(root) = uqa_sql::semantics::partition::partition_hierarchy_root(
                    context.constraints.partitions.catalog,
                    &prepared.table,
                )? {
                    reject_partition_rewrite(&context, &prepared, &root, params, true)?;
                }
                self.overlay
                    .as_mut()
                    .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
                    .note_update(context.constraints, &existing, &prepared.new_document)?;
                Ok(PreparedInsertConflict::Updated(prepared))
            }
        }
    }
}
