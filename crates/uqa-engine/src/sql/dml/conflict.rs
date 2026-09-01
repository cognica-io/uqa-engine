//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT conflict resolution, identity extraction, and RETURNING assembly.

use super::{
    bind_projection_output_schema, build_projection_physical_row_with_ctes,
    dml_append_hidden_qualified_row, dml_storage_error, dml_target_row, doc_id_value,
    eval_mutation_assignment, eval_mutation_expr, key_constraint_values,
    lock_document_key_dependencies, lock_mutation_target, missing_document_error,
    prepare_document_rewrite, reject_partition_rewrite, update_lock_strength, BTreeSet,
    ConflictActionPlan, ConflictPlan, CteScope, DocId, Document, Engine, MutationAssignmentTarget,
    MutationLockCleanup, MutationLockTarget, MutationRowImage, MutationRowImages,
    PhysicalDocumentIdentity, PreparedInsertConflict, ProjectionPlan, SQLError, SQLParam,
    SQLResult, Value, DOC_ID_COLUMN, TABLE_OID_COLUMN,
};
use rusqlite::OptionalExtension;
use uqa_execution::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema};
use uqa_sql::ast::{ReturningAliases, Statement};

enum CurrentInsertConflict {
    Overlay,
    Base(PhysicalDocumentIdentity),
}

pub(in crate::sql) struct InsertConflictPreparation<'a> {
    pub(in crate::sql) engine: &'a Engine,
    pub(in crate::sql) table: &'a str,
    pub(in crate::sql) target_qualifier: &'a str,
    pub(in crate::sql) on_conflict: &'a ConflictPlan,
    pub(in crate::sql) document: &'a Document,
    pub(in crate::sql) params: &'a [SQLParam],
    pub(in crate::sql) scope: &'a CteScope,
}

/// Disk-backed view of rows inserted or rewritten earlier by the same INSERT command. `PostgreSQL` resolves `ON CONFLICT` inputs sequentially: a key moved away by an earlier update becomes insertable, while a later `DO UPDATE` that reaches a row already inserted or updated by the command raises 21000. Keeping the exact key index in a temporary `SQLite` database preserves those semantics without retaining a cardinality-sized map above `work_mem`.
struct InsertConflictOverlay {
    connection: rusqlite::Connection,
    _directory: tempfile::TempDir,
    constraints: Vec<uqa_sql::ast::TableKeyConstraint>,
    relevant_constraints: Vec<usize>,
    next_insert_identity: u64,
}

impl InsertConflictOverlay {
    fn new(engine: &Engine, table: &str, on_conflict: &ConflictPlan) -> Result<Self, SQLError> {
        let constraints = engine
            .try_key_constraints(table)
            .map_err(|error| dml_storage_error("INSERT conflict overlay", error))?;
        let relevant_constraints = if on_conflict.conflict_columns.is_empty() {
            (0..constraints.len()).collect()
        } else {
            let target = on_conflict
                .conflict_columns
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            let index = constraints
                .iter()
                .position(|constraint| {
                    constraint.columns.len() == target.len()
                        && constraint
                            .columns
                            .iter()
                            .map(String::as_str)
                            .collect::<BTreeSet<_>>()
                            == target
                })
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!(
                        "ON CONFLICT target ({}) does not match a PRIMARY KEY or UNIQUE constraint",
                        on_conflict.conflict_columns.join(", ")
                    ))
                })?;
            vec![index]
        };
        let directory = tempfile::Builder::new()
            .prefix("uqa-insert-conflict-")
            .tempdir()
            .map_err(|error| {
                SQLError::Internal(format!("create INSERT conflict overlay directory: {error}"))
            })?;
        let connection = rusqlite::Connection::open(directory.path().join("overlay.sqlite"))
            .map_err(|error| {
                SQLError::Internal(format!("open INSERT conflict overlay: {error}"))
            })?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = OFF;
                 PRAGMA synchronous = OFF;
                 CREATE TABLE overlay_keys (
                     physical_table TEXT NOT NULL,
                     constraint_index INTEGER NOT NULL,
                     key BLOB NOT NULL,
                     identity BLOB NOT NULL,
                     PRIMARY KEY (physical_table, constraint_index, key)
                 ) WITHOUT ROWID;
                 CREATE TABLE overridden_documents (
                     physical_table TEXT NOT NULL,
                     doc_id BLOB NOT NULL,
                     PRIMARY KEY (physical_table, doc_id)
                 ) WITHOUT ROWID;",
            )
            .map_err(|error| {
                SQLError::Internal(format!("initialize INSERT conflict overlay: {error}"))
            })?;
        Ok(Self {
            connection,
            _directory: directory,
            constraints,
            relevant_constraints,
            next_insert_identity: 0,
        })
    }

    fn find(
        &self,
        engine: &Engine,
        table: &str,
        document: &Document,
    ) -> Result<Option<CurrentInsertConflict>, SQLError> {
        for &index in &self.relevant_constraints {
            let constraint = &self.constraints[index];
            let constraint_index = i64::try_from(index).map_err(|_| {
                SQLError::Internal("INSERT conflict constraint index exceeds i64".into())
            })?;
            let Some(values) = key_constraint_values(constraint, document) else {
                continue;
            };
            let key = uqa_execution::canonical_row_key(&values)
                .map_err(crate::sql::select::physical_exec_error)?;
            let overlay = self
                .connection
                .query_row(
                    "SELECT 1 FROM overlay_keys WHERE physical_table = ?1 AND constraint_index = ?2 AND key = ?3",
                    rusqlite::params![table, constraint_index, key],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|error| {
                    SQLError::Internal(format!("probe INSERT conflict overlay: {error}"))
                })?;
            if overlay.is_some() {
                return Ok(Some(CurrentInsertConflict::Overlay));
            }
            let Some(doc_id) = engine.find_conflict(table, &constraint.columns, &values)? else {
                continue;
            };
            let overridden = self
                .connection
                .query_row(
                    "SELECT 1 FROM overridden_documents WHERE physical_table = ?1 AND doc_id = ?2",
                    rusqlite::params![table, doc_id.to_be_bytes().as_slice()],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "probe overridden INSERT conflict document: {error}"
                    ))
                })?;
            if overridden.is_none() {
                return Ok(Some(CurrentInsertConflict::Base(
                    PhysicalDocumentIdentity {
                        table: table.to_string(),
                        doc_id,
                    },
                )));
            }
        }
        Ok(None)
    }

    fn note_insert(&mut self, table: &str, document: &Document) -> Result<(), SQLError> {
        let mut identity = Vec::with_capacity(9);
        identity.push(b'i');
        identity.extend_from_slice(&self.next_insert_identity.to_be_bytes());
        self.next_insert_identity = self.next_insert_identity.checked_add(1).ok_or_else(|| {
            SQLError::Internal("INSERT conflict overlay identity space is exhausted".into())
        })?;
        self.note_keys(table, &identity, document)
    }

    fn note_update(
        &mut self,
        base: &PhysicalDocumentIdentity,
        document: &Document,
    ) -> Result<(), SQLError> {
        let mut overlay_identity = Vec::with_capacity(9);
        overlay_identity.push(b'b');
        overlay_identity.extend_from_slice(&base.doc_id.to_be_bytes());
        self.connection
            .execute(
                "INSERT OR IGNORE INTO overridden_documents (physical_table, doc_id) VALUES (?1, ?2)",
                rusqlite::params![base.table, base.doc_id.to_be_bytes().as_slice()],
            )
            .map_err(|error| {
                SQLError::Internal(format!(
                    "record overridden INSERT conflict document: {error}"
                ))
            })?;
        self.note_keys(&base.table, &overlay_identity, document)
    }

    fn note_keys(
        &mut self,
        table: &str,
        identity: &[u8],
        document: &Document,
    ) -> Result<(), SQLError> {
        for (index, constraint) in self.constraints.iter().enumerate() {
            let constraint_index = i64::try_from(index).map_err(|_| {
                SQLError::Internal("INSERT conflict constraint index exceeds i64".into())
            })?;
            let Some(values) = key_constraint_values(constraint, document) else {
                continue;
            };
            let key = uqa_execution::canonical_row_key(&values)
                .map_err(crate::sql::select::physical_exec_error)?;
            self.connection
                .execute(
                    "INSERT OR IGNORE INTO overlay_keys (physical_table, constraint_index, key, identity) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![table, constraint_index, key, identity],
                )
                .map_err(|error| {
                    SQLError::Internal(format!("record INSERT conflict overlay key: {error}"))
                })?;
        }
        Ok(())
    }
}

fn on_conflict_cardinality_violation() -> SQLError {
    SQLError::Routine {
        sqlstate: "21000".into(),
        message: "ON CONFLICT DO UPDATE command cannot affect row a second time\nHINT: Ensure that no rows proposed for insertion within the same command have duplicate constrained values.".into(),
    }
}

pub(in crate::sql) fn find_insert_conflict(
    engine: &Engine,
    table: &str,
    on_conflict: &ConflictPlan,
    document: &Document,
) -> Result<Option<PhysicalDocumentIdentity>, SQLError> {
    let constraints = engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("INSERT conflict lookup", err))?;
    if !on_conflict.conflict_columns.is_empty() {
        let target: BTreeSet<&str> = on_conflict
            .conflict_columns
            .iter()
            .map(String::as_str)
            .collect();
        let constraint = constraints
            .iter()
            .find(|constraint| {
                constraint.columns.len() == target.len()
                    && constraint
                        .columns
                        .iter()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>()
                        == target
            })
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "ON CONFLICT target ({}) does not match a PRIMARY KEY or UNIQUE constraint",
                    on_conflict.conflict_columns.join(", ")
                ))
            })?;
        let Some(conflict_values) = key_constraint_values(constraint, document) else {
            return Ok(None);
        };
        return Ok(engine
            .find_conflict(table, &constraint.columns, &conflict_values)?
            .map(|doc_id| PhysicalDocumentIdentity {
                table: table.to_string(),
                doc_id,
            }));
    }

    for constraint in &constraints {
        let Some(values) = key_constraint_values(constraint, document) else {
            continue;
        };
        if let Some(doc_id) = engine.find_conflict(table, &constraint.columns, &values)? {
            return Ok(Some(PhysicalDocumentIdentity {
                table: table.to_string(),
                doc_id,
            }));
        }
    }
    Ok(None)
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
fn build_conflict_update(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    existing_id: DocId,
    document: &Document,
    assignments: &[uqa_planner::AssignmentPlan],
    predicate: Option<&uqa_execution::ScalarExpr>,
    params: &[SQLParam],
    scope: &CteScope,
) -> Result<BuiltConflictUpdate, SQLError> {
    let existing_doc = engine
        .get_document_for_mutation(table, existing_id)?
        .ok_or_else(|| missing_document_error("INSERT ON CONFLICT", table, existing_id))?;
    let target_row = dml_target_row(engine, table, target_qualifier, existing_id, &existing_doc)?;
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("INSERT EXCLUDED schema", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut excluded_document = document.clone();
    crate::engine_generated::materialize_virtual_generated_columns(
        &definitions,
        &mut excluded_document,
    )?;
    let excluded_columns = if definitions.is_empty() {
        excluded_document
            .keys()
            .filter(|column| {
                column.as_str() != crate::sql::XMIN_STORAGE_COLUMN
                    && column.as_str() != crate::sql::XMIN_USER_STORAGE_COLUMN
            })
            .cloned()
            .collect::<Vec<_>>()
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
        let keep = eval_mutation_expr(engine, scope, predicate, Some(&conflict_row), params)?;
        if !uqa_sql::expr::truthy(&keep) {
            return Ok(BuiltConflictUpdate::Skip);
        }
    }
    let mut updated_doc = existing_doc.clone();
    for assignment in assignments {
        let value = eval_mutation_assignment(
            engine,
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
pub(in crate::sql) struct InsertConflictLocks {
    transient: MutationLockCleanup,
    overlay: Option<InsertConflictOverlay>,
}

impl InsertConflictLocks {
    pub(in crate::sql) fn new(engine: &Engine) -> Self {
        Self {
            transient: MutationLockCleanup::new(engine),
            overlay: None,
        }
    }

    pub(in crate::sql) fn lock_document(
        &mut self,
        engine: &Engine,
        table: &str,
        target_qualifier: &str,
        on_conflict: &ConflictPlan,
        document: &Document,
    ) -> Result<(), SQLError> {
        for _ in 0..=64 {
            let Some(existing) = find_insert_conflict(engine, table, on_conflict, document)? else {
                return Ok(());
            };
            let (locked, recheck) = match &on_conflict.action {
                ConflictActionPlan::Nothing => (
                    existing.clone(),
                    self.transient.acquire(
                        engine,
                        &existing.table,
                        target_qualifier,
                        existing.doc_id,
                        uqa_sql::ast::LockStrength::ForKeyShare,
                    )?,
                ),
                ConflictActionPlan::Update { assignments, .. } => {
                    match lock_mutation_target(
                        engine,
                        &existing.table,
                        target_qualifier,
                        existing.doc_id,
                        update_lock_strength(
                            engine,
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
                            engine.refresh_explicit_statement_snapshot()?;
                            continue;
                        }
                    }
                }
            };
            if recheck {
                engine.refresh_explicit_statement_snapshot()?;
            }
            if find_insert_conflict(engine, table, on_conflict, document)? == Some(locked) {
                return Ok(());
            }
        }
        Err(SQLError::Internal(format!(
            "INSERT conflict lookup for `{table}` did not converge"
        )))
    }

    #[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
    pub(in crate::sql) fn prepare_document(
        &mut self,
        preparation: InsertConflictPreparation<'_>,
        referential_actions: &mut super::ReferentialActionContext,
    ) -> Result<PreparedInsertConflict, SQLError> {
        let InsertConflictPreparation {
            engine,
            table,
            target_qualifier,
            on_conflict,
            document,
            params,
            scope,
        } = preparation;
        let key_acquisitions = lock_document_key_dependencies(engine, table, document, None)?;
        if self.overlay.is_none() {
            self.overlay = Some(InsertConflictOverlay::new(engine, table, on_conflict)?);
        }
        let current = self
            .overlay
            .as_ref()
            .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
            .find(engine, table, document)?;
        match current {
            None => {
                self.overlay
                    .as_mut()
                    .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
                    .note_insert(table, document)?;
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
        self.lock_document(engine, table, target_qualifier, on_conflict, document)?;
        let current = self
            .overlay
            .as_ref()
            .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
            .find(engine, table, document)?;
        let existing = match current {
            None => {
                self.overlay
                    .as_mut()
                    .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
                    .note_insert(table, document)?;
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
            engine,
            &existing.table,
            target_qualifier,
            existing.doc_id,
            document,
            assignments,
            predicate.as_ref(),
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
                let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
                    engine,
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
                    engine,
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
                if let Some(root) = engine.partition_hierarchy_root(&prepared.table)? {
                    reject_partition_rewrite(engine, &prepared, &root, params, true)?;
                }
                self.overlay
                    .as_mut()
                    .ok_or_else(|| SQLError::Internal("INSERT conflict overlay is absent".into()))?
                    .note_update(&existing, &prepared.new_document)?;
                Ok(PreparedInsertConflict::Updated(prepared))
            }
        }
    }
}

pub(in crate::sql) fn validate_returning_alias_relations(
    target_qualifier: &str,
    aliases: &ReturningAliases,
    supplemental: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let mut relation_names = BTreeSet::from([target_qualifier]);
    for (alias, explicit) in [
        (aliases.old.as_str(), aliases.old_explicit),
        (aliases.new.as_str(), aliases.new_explicit),
    ] {
        if !explicit {
            continue;
        }
        if relation_names.contains(alias)
            || supplemental.is_some_and(|schema| schema.has_qualifier(alias))
        {
            return Err(SQLError::Routine {
                sqlstate: "42712".into(),
                message: format!("table name \"{alias}\" specified more than once"),
            });
        }
        relation_names.insert(alias);
    }
    Ok(())
}

pub(in crate::sql) fn returning_row_context(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    images: MutationRowImages<'_>,
    aliases: &ReturningAliases,
) -> Result<OwnedPhysicalRow, SQLError> {
    let current = images.new.as_ref().or(images.old.as_ref()).ok_or_else(|| {
        SQLError::Internal(format!(
            "RETURNING for table `{table}` has neither an old nor a new row image"
        ))
    })?;
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let target = returning_target_schema(engine, table)?;
    let mut columns = target.columns().to_vec();
    let mut types = target.column_types().to_vec();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(uqa_sql::ast::ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(uqa_sql::ast::ColumnType::Oid));
    columns.push(crate::sql::XMIN_COLUMN.into());
    types.push(Some(uqa_sql::ast::ColumnType::Xid));
    let schema = returning_context_schema(&columns, &types, target_qualifier, aliases);
    let current_values = returning_image_values(engine, Some(current), &columns, &definitions)?;
    let old_values = returning_image_values(engine, images.old.as_ref(), &columns, &definitions)?;
    let new_values = returning_image_values(engine, images.new.as_ref(), &columns, &definitions)?;
    let values = current_values
        .into_iter()
        .chain(old_values)
        .chain(new_values)
        .collect();
    Ok(OwnedPhysicalRow::new(
        schema,
        PhysicalRow::from_values(values),
    ))
}

mod returning;
pub(in crate::sql) use returning::{
    build_returning_row, build_returning_value_row, dml_command_returning_schema,
    dml_returning_result, dml_returning_result_with_projections, dml_statement_returning_schema,
    document_supplied_id, expanded_returning_projections, returning_expression_schema,
    returning_target_schema, returning_value_context, DmlReturningShape, ReturningProjectionRow,
    ReturningValueProjectionRow,
};
use returning::{returning_context_schema, returning_image_values};
