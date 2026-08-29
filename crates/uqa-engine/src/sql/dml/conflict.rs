//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT conflict resolution, identity extraction, and RETURNING assembly.

use std::sync::Arc;

use super::{
    bind_projection_output_schema, build_projection_physical_row_with_ctes, decode_prepared_doc_id,
    decode_prepared_document_rewrite, dml_append_hidden_qualified_row, dml_storage_error,
    dml_target_row, doc_id_value, encode_prepared_doc_id, encode_prepared_document_rewrite,
    eval_mutation_assignment, eval_mutation_expr, finalize_partition_rewrite,
    key_constraint_values, lock_document_key_dependencies, lock_mutation_target,
    missing_document_error, prepare_document_rewrite, update_lock_strength, BTreeMap, BTreeSet,
    ConflictActionPlan, ConflictPlan, CteScope, DocId, Document, Engine, MutationAssignmentTarget,
    MutationLockTarget, PartitionRewritePolicy, PhysicalDocumentIdentity, PreparedDocumentRewrite,
    ProjectionPlan, SQLError, SQLParam, SQLResult, Value, DOC_ID_COLUMN,
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
            if target.len() != on_conflict.conflict_columns.len() {
                return Err(SQLError::TypeMismatch(format!(
                    "ON CONFLICT target ({}) names a column more than once",
                    on_conflict.conflict_columns.join(", ")
                )));
            }
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
        if target.len() != on_conflict.conflict_columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "ON CONFLICT target ({}) names a column more than once",
                on_conflict.conflict_columns.join(", ")
            )));
        }
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

pub(in crate::sql) enum PreparedInsertConflict {
    Unresolved,
    Insert { doc_id: DocId, supplied: bool },
    Skip,
    Updated(PreparedDocumentRewrite),
}

pub(in crate::sql) fn encode_prepared_insert_conflict(prepared: PreparedInsertConflict) -> Value {
    match prepared {
        PreparedInsertConflict::Unresolved => Value::Str("unresolved".into()),
        PreparedInsertConflict::Insert { doc_id, supplied } => Value::Map(BTreeMap::from([
            ("kind".into(), Value::Str("insert".into())),
            ("doc_id".into(), encode_prepared_doc_id(doc_id)),
            ("supplied".into(), Value::Bool(supplied)),
        ])),
        PreparedInsertConflict::Skip => Value::Str("skip".into()),
        PreparedInsertConflict::Updated(rewrite) => Value::Map(BTreeMap::from([
            ("kind".into(), Value::Str("updated".into())),
            ("rewrite".into(), encode_prepared_document_rewrite(rewrite)),
        ])),
    }
}

pub(in crate::sql) fn decode_prepared_insert_conflict(
    value: Value,
) -> Result<PreparedInsertConflict, SQLError> {
    match value {
        Value::Str(kind) if kind == "unresolved" => Ok(PreparedInsertConflict::Unresolved),
        Value::Str(kind) if kind == "skip" => Ok(PreparedInsertConflict::Skip),
        Value::Map(mut fields) => match fields.remove("kind") {
            Some(Value::Str(kind)) if kind == "insert" => Ok(PreparedInsertConflict::Insert {
                doc_id: decode_prepared_doc_id(
                    fields.remove("doc_id").ok_or_else(|| {
                        SQLError::Internal(
                            "prepared INSERT payload has no document identity".into(),
                        )
                    })?,
                    "prepared INSERT action",
                )?,
                supplied: match fields.remove("supplied") {
                    Some(Value::Bool(supplied)) => supplied,
                    _ => {
                        return Err(SQLError::Internal(
                            "prepared INSERT payload has no supplied-id flag".into(),
                        ))
                    }
                },
            }),
            Some(Value::Str(kind)) if kind == "updated" => Ok(PreparedInsertConflict::Updated(
                decode_prepared_document_rewrite(fields.remove("rewrite").ok_or_else(|| {
                    SQLError::Internal(
                        "prepared INSERT conflict payload has no rewrite plan".into(),
                    )
                })?)?,
            )),
            _ => Err(SQLError::Internal(
                "prepared INSERT conflict payload has an unknown kind".into(),
            )),
        },
        _ => Err(SQLError::Internal(
            "prepared INSERT conflict payload has an invalid representation".into(),
        )),
    }
}

enum BuiltConflictUpdate {
    Skip,
    Update {
        old_document: Document,
        new_document: Document,
    },
}

#[allow(clippy::too_many_arguments)]
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

struct TransientConflictLocks {
    manager: Arc<crate::row_locks::RowLockManager>,
    acquisitions: Vec<crate::row_locks::RowLockAcquisition>,
}

impl TransientConflictLocks {
    fn new(engine: &Engine) -> Self {
        Self {
            manager: Arc::clone(&engine.row_locks),
            acquisitions: Vec::new(),
        }
    }

    fn lock(
        &mut self,
        engine: &Engine,
        table: &str,
        display_name: &str,
        doc_id: DocId,
    ) -> Result<bool, SQLError> {
        match engine.lock_row(
            table,
            doc_id,
            uqa_sql::ast::LockStrength::ForKeyShare,
            uqa_sql::ast::LockWait::Block,
            display_name,
        )? {
            crate::row_locks::LockAcquire::Granted {
                acquisition,
                waited,
                ..
            } => {
                self.acquisitions.extend(acquisition);
                Ok(waited)
            }
            crate::row_locks::LockAcquire::Skipped => Err(SQLError::Internal(
                "blocking INSERT conflict wait unexpectedly skipped a row".into(),
            )),
        }
    }
}

/// Locks every currently visible ON CONFLICT dependency for an INSERT input set while the storage transaction is still a reader. DO NOTHING locks are retained only until writer promotion; DO UPDATE target locks keep their normal transaction lifetime. Once the single backend writer is held no concurrent transaction can create a new physical conflict and make the execution phase wait behind a tuple owner.
pub(in crate::sql) struct InsertConflictLocks {
    transient: TransientConflictLocks,
    overlay: Option<InsertConflictOverlay>,
}

impl InsertConflictLocks {
    pub(in crate::sql) fn new(engine: &Engine) -> Self {
        Self {
            transient: TransientConflictLocks::new(engine),
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
                    self.transient.lock(
                        engine,
                        &existing.table,
                        target_qualifier,
                        existing.doc_id,
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
                rollback_lock_acquisitions(engine, key_acquisitions);
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
                rollback_lock_acquisitions(engine, key_acquisitions);
                return match &on_conflict.action {
                    ConflictActionPlan::Nothing => Ok(PreparedInsertConflict::Skip),
                    ConflictActionPlan::Update { .. } => Err(on_conflict_cardinality_violation()),
                };
            }
            Some(CurrentInsertConflict::Base(identity)) => identity,
        };
        self.transient.acquisitions.extend(key_acquisitions);
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
                let mut prepared = prepare_document_rewrite(
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
                    finalize_partition_rewrite(
                        engine,
                        &mut prepared,
                        &root,
                        params,
                        true,
                        PartitionRewritePolicy::RejectOnConflict,
                    )?;
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

impl Drop for TransientConflictLocks {
    fn drop(&mut self) {
        for acquisition in self.acquisitions.drain(..).rev() {
            self.manager.rollback_acquisition(acquisition);
        }
    }
}

fn rollback_lock_acquisitions(
    engine: &Engine,
    acquisitions: Vec<crate::row_locks::RowLockAcquisition>,
) {
    for acquisition in acquisitions.into_iter().rev() {
        engine.rollback_row_lock_acquisition(acquisition);
    }
}

#[derive(Clone, Copy)]
pub(in crate::sql) struct ReturningRowImage<'a> {
    pub doc_id: DocId,
    pub document: &'a Document,
}

#[derive(Clone, Copy)]
pub(in crate::sql) struct ReturningRowImages<'a> {
    pub old: Option<ReturningRowImage<'a>>,
    pub new: Option<ReturningRowImage<'a>>,
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
    images: ReturningRowImages<'_>,
    aliases: &ReturningAliases,
) -> Result<OwnedPhysicalRow, SQLError> {
    let current = images.new.or(images.old).ok_or_else(|| {
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
    columns.push(crate::sql::XMIN_COLUMN.into());
    types.push(Some(uqa_sql::ast::ColumnType::Xid));
    let schema = returning_context_schema(&columns, &types, target_qualifier, aliases);
    let current_values = returning_image_values(Some(current), &columns, &definitions)?;
    let old_values = returning_image_values(images.old, &columns, &definitions)?;
    let new_values = returning_image_values(images.new, &columns, &definitions)?;
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

fn returning_image_values(
    image: Option<ReturningRowImage<'_>>,
    columns: &[String],
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Result<Vec<Value>, SQLError> {
    let Some(image) = image else {
        return Ok(vec![Value::Null; columns.len()]);
    };
    let mut document = image.document.clone();
    crate::engine_generated::materialize_virtual_generated_columns(definitions, &mut document)?;
    columns
        .iter()
        .map(|column| {
            if super::is_virtual_document_id_column(column, definitions)
                || definitions.iter().any(|definition| {
                    definition.name == *column
                        && definition.primary_key
                        && definition.ty.is_integer()
                })
            {
                doc_id_value(image.doc_id)
            } else {
                Ok(document.get(column).cloned().unwrap_or(Value::Null))
            }
        })
        .collect()
}

fn returning_context_schema(
    columns: &[String],
    types: &[Option<uqa_sql::ast::ColumnType>],
    target_qualifier: &str,
    aliases: &ReturningAliases,
) -> RowSchema {
    let target =
        RowSchema::with_qualified_types(target_qualifier, columns.to_vec(), types.to_vec());
    let hidden_types = types
        .iter()
        .cloned()
        .chain(types.iter().cloned())
        .collect::<Vec<_>>();
    let schema = RowSchema::append_hidden_typed(&target, &hidden_types);
    let width = columns.len();
    let identity_aliases = columns
        .iter()
        .enumerate()
        .flat_map(|(position, column)| {
            [
                (
                    ColumnIdentity::qualified(&aliases.old, column),
                    width + position,
                    types[position].clone(),
                ),
                (
                    ColumnIdentity::qualified(&aliases.new, column),
                    width * 2 + position,
                    types[position].clone(),
                ),
            ]
        })
        .collect::<Vec<_>>();
    RowSchema::with_physical_identity_aliases(&schema, &identity_aliases)
}

#[derive(Clone, Copy)]
pub(in crate::sql) struct ReturningProjectionRow<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub images: ReturningRowImages<'a>,
    pub aliases: &'a ReturningAliases,
    pub context: Option<&'a OwnedPhysicalRow>,
}

pub(in crate::sql) struct ReturningValueProjectionRow<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub current: &'a [Value],
    pub old: Option<&'a [Value]>,
    pub new: Option<&'a [Value]>,
    pub aliases: &'a ReturningAliases,
    pub context: Option<&'a OwnedPhysicalRow>,
}

pub(in crate::sql) fn build_returning_row(
    engine: &Engine,
    input: ReturningProjectionRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<OwnedPhysicalRow, SQLError> {
    let row = returning_projection_context(engine, input)?;
    let projections = expanded_returning_projections(
        engine,
        input.table,
        input.target_qualifier,
        input.aliases,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, &snapshot_scope)
}

/// Project a RETURNING row supplied positionally by a rewrite-rule action.
/// Rule target lists describe the event relation's row type but are not
/// storage documents, so integer primary-key values must remain ordinary
/// values rather than being reconstructed from an internal document id.
pub(in crate::sql) fn build_returning_value_row(
    engine: &Engine,
    input: ReturningValueProjectionRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<OwnedPhysicalRow, SQLError> {
    let target = returning_target_schema(engine, input.table)?;
    let width = target.len();
    if input.current.len() != width
        || input.old.is_some_and(|row| row.len() != width)
        || input.new.is_some_and(|row| row.len() != width)
    {
        return Err(SQLError::Internal(
            "rewrite-rule RETURNING row does not match the event relation".into(),
        ));
    }
    let mut columns = target.columns().to_vec();
    let mut types = target.column_types().to_vec();
    let append_doc_id = !columns.iter().any(|column| column == DOC_ID_COLUMN);
    if append_doc_id {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(uqa_sql::ast::ColumnType::BigInteger));
    }
    let image = |row: Option<&[Value]>| {
        let mut values = row.map_or_else(|| vec![Value::Null; width], <[Value]>::to_vec);
        if append_doc_id {
            values.push(Value::Null);
        }
        values
    };
    let mut current = input.current.to_vec();
    if append_doc_id {
        current.push(Value::Null);
    }
    let schema = returning_context_schema(&columns, &types, input.target_qualifier, input.aliases);
    let values = current
        .into_iter()
        .chain(image(input.old))
        .chain(image(input.new))
        .collect();
    let target = OwnedPhysicalRow::new(schema, PhysicalRow::from_values(values));
    let row = input.context.map_or(target.clone(), |context| {
        OwnedPhysicalRow::new(
            RowSchema::join(&target.schema, &context.schema, std::iter::empty()),
            PhysicalRow::concat(&target.row, &context.row),
        )
    });
    let projections = expanded_returning_projections(
        engine,
        input.table,
        input.target_qualifier,
        input.aliases,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, &snapshot_scope)
}

pub(in crate::sql) fn returning_projection_context(
    engine: &Engine,
    input: ReturningProjectionRow<'_>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let target = returning_row_context(
        engine,
        input.table,
        input.target_qualifier,
        input.images,
        input.aliases,
    )?;
    let row = input.context.map_or(target.clone(), |context| {
        OwnedPhysicalRow::new(
            RowSchema::join(&target.schema, &context.schema, std::iter::empty()),
            PhysicalRow::concat(&target.row, &context.row),
        )
    });
    Ok(row)
}

pub(in crate::sql) struct DmlReturningShape<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub aliases: &'a ReturningAliases,
    pub returning: &'a [ProjectionPlan],
    pub params: &'a [SQLParam],
    pub ctes: &'a CteScope,
    pub supplemental_schema: Option<&'a RowSchema>,
}

/// Derive a DML statement's declared RETURNING row type without executing the
/// statement. Rewrite-rule registration uses this to enforce `PostgreSQL`'s
/// positional event-row contract before the rule reaches durable storage.
pub(in crate::sql) fn dml_statement_returning_schema(
    engine: &Engine,
    statement: Statement,
) -> Result<Option<RowSchema>, SQLError> {
    let plan = crate::sql::lower_statement(engine, statement);
    let uqa_planner::UnifiedPlan::Command(command) = plan else {
        return Ok(None);
    };
    match command.as_ref() {
        uqa_planner::CommandPlan::Insert(plan) => analyze_dml_returning_plan(
            engine,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            &plan.ctes,
            None,
            &plan.subqueries,
        ),
        uqa_planner::CommandPlan::Update(plan) => analyze_dml_returning_plan(
            engine,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            &plan.ctes,
            plan.source.as_deref(),
            &plan.subqueries,
        ),
        uqa_planner::CommandPlan::Delete(plan) => analyze_dml_returning_plan(
            engine,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            &plan.ctes,
            plan.source.as_deref(),
            &plan.subqueries,
        ),
        _ => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze_dml_returning_plan(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    returning: &[ProjectionPlan],
    cte_plans: &[uqa_planner::CtePlan],
    source: Option<&uqa_planner::SourcePlan>,
    subqueries: &[uqa_planner::QueryPlan],
) -> Result<Option<RowSchema>, SQLError> {
    if returning.is_empty() {
        return Ok(None);
    }
    let mut ctes = CteScope::new_for_current_routine();
    for plan in cte_plans {
        ctes.insert_deferred(plan.clone());
    }
    ctes.scalar_subqueries = subqueries.to_vec();
    let supplemental = source
        .map(|source| {
            crate::sql::select::analyze_source_plan_schema(engine, source, &[], &ctes, None)
        })
        .transpose()?;
    let star_schema = returning_target_schema(engine, table)?;
    let expression_schema = returning_expression_schema(
        &star_schema,
        target_qualifier,
        aliases,
        supplemental.as_ref(),
    );
    let projections =
        expanded_returning_projections(engine, table, target_qualifier, aliases, returning)?;
    crate::sql::select::analyze_projection_output_schema(
        engine,
        &projections,
        &expression_schema,
        &star_schema,
        subqueries,
        &[],
        &ctes,
    )
    .map(Some)
}

pub(in crate::sql) fn dml_returning_result(
    engine: &Engine,
    shape: DmlReturningShape<'_>,
    rows: Vec<OwnedPhysicalRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    let projections = expanded_returning_projections(
        engine,
        shape.table,
        shape.target_qualifier,
        shape.aliases,
        shape.returning,
    )?;
    dml_returning_result_with_projections(engine, shape, &projections, rows, affected_rows)
}

pub(in crate::sql) fn dml_returning_result_with_projections(
    engine: &Engine,
    shape: DmlReturningShape<'_>,
    projections: &[ProjectionPlan],
    rows: Vec<OwnedPhysicalRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    let star_schema = returning_target_schema(engine, shape.table)?;
    let expression_schema = returning_expression_schema(
        &star_schema,
        shape.target_qualifier,
        shape.aliases,
        shape.supplemental_schema,
    );
    let output = bind_projection_output_schema(
        engine,
        projections,
        &expression_schema,
        &star_schema,
        &shape.ctes.scalar_subqueries,
        shape.params,
        shape.ctes,
    )?;
    let preserve_positions =
        output.columns().iter().collect::<BTreeSet<_>>().len() != output.columns().len();
    let mut named_rows = Vec::with_capacity(rows.len());
    let mut positional_rows = preserve_positions.then(|| Vec::with_capacity(rows.len()));
    for row in rows {
        if let Some(positional_rows) = positional_rows.as_mut() {
            positional_rows.push(row.view().iter().map(|(_, value)| value.clone()).collect());
        }
        named_rows.push(row.into_result_row());
    }
    let mut result = SQLResult::from_typed_rows_with_positions(
        output.columns().to_vec(),
        output.column_types().to_vec(),
        named_rows,
        positional_rows,
    );
    result.affected_rows = affected_rows;
    Ok(result)
}

fn returning_target_schema(engine: &Engine, table: &str) -> Result<RowSchema, SQLError> {
    let definitions = engine
        .try_describe_table_row_type(table)
        .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    if definitions.is_empty() {
        let columns = engine
            .try_table_columns(table)
            .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?;
        let width = columns.len();
        return Ok(RowSchema::with_types(columns, vec![None; width]));
    }
    let columns = definitions
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    let types = definitions
        .into_iter()
        .map(|definition| Some(definition.ty))
        .collect();
    Ok(RowSchema::with_types(columns, types))
}

fn returning_expression_schema(
    target: &RowSchema,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    supplemental: Option<&RowSchema>,
) -> RowSchema {
    let mut columns = target.columns().to_vec();
    let mut types = target.column_types().to_vec();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(uqa_sql::ast::ColumnType::BigInteger));
    }
    columns.push(crate::sql::XMIN_COLUMN.into());
    types.push(Some(uqa_sql::ast::ColumnType::Xid));
    let target = returning_context_schema(&columns, &types, target_qualifier, aliases);
    supplemental.map_or(target.clone(), |source| {
        RowSchema::join(&target, source, std::iter::empty())
    })
}

pub(in crate::sql) fn expanded_returning_projections(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    returning: &[ProjectionPlan],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let columns = returning_target_schema(engine, table)?.columns().to_vec();
    let mut projections = Vec::with_capacity(returning.len().max(columns.len()));
    for projection in returning {
        match &projection.expr {
            uqa_execution::ScalarExpr::Star => {
                projections.extend(columns.iter().map(|column| ProjectionPlan {
                    expr: uqa_execution::ScalarExpr::Column(column.clone()),
                    alias: Some(column.clone()),
                }));
            }
            uqa_execution::ScalarExpr::QualifiedStar(qualifier)
                if qualifier == target_qualifier
                    || qualifier == &aliases.old
                    || qualifier == &aliases.new =>
            {
                projections.extend(columns.iter().map(|column| ProjectionPlan {
                    expr: uqa_execution::ScalarExpr::QualifiedColumn {
                        qualifier: qualifier.clone(),
                        column: column.clone(),
                    },
                    alias: Some(column.clone()),
                }));
            }
            _ => projections.push(projection.clone()),
        }
    }
    Ok(projections)
}

pub(in crate::sql) fn document_supplied_id(
    document: &Document,
    id_column: &str,
    auto_increment: bool,
) -> Result<Option<DocId>, SQLError> {
    match document.get(id_column) {
        Some(Value::Int(value)) if *value >= 0 => Ok(Some(*value as DocId)),
        Some(Value::Null) | None => Ok(None),
        Some(other) if auto_increment => Err(SQLError::TypeMismatch(format!(
            "auto-increment id must be an integer, got {other:?}"
        ))),
        Some(_) => Ok(None),
    }
}

// -------------------------------------------------------------------------

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------
