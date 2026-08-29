//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::scalar::{eval_lowered_expression, eval_physical_scalar, PhysicalEvalContext};
use super::ScopedEngineHook;
use super::{
    bind_projection_output_schema, build_join_spill_with_ctes,
    build_projection_physical_row_with_ctes, coerce_to_column_type, column_type_name, doc_id_value,
    partition_insert_target, validate_vector_dimensions, value_to_tensor, value_to_vector,
    BTreeMap, BTreeSet, BinaryOp, ColumnType, CteScope, DocId, Document, Engine, ForeignKey,
    ForeignKeyAction, ForeignKeyMatch, RowIndependentUpdateValues, SQLError, SQLParam, SQLResult,
    Value, DOC_ID_COLUMN, XMIN_COLUMN, XMIN_STORAGE_COLUMN, XMIN_USER_STORAGE_COLUMN,
};
use uqa_execution::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema, ScalarExpr};
use uqa_planner::{
    ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan,
    ProjectionPlan, SourcePlan, UpdatePlan,
};

fn dml_storage_error(action: &str, err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

pub(in crate::sql) fn concurrent_update_serialization_failure() -> SQLError {
    SQLError::Routine {
        sqlstate: "40001".into(),
        message: "could not serialize access due to concurrent update".into(),
    }
}

pub(in crate::sql) struct DmlCommandMutationOverlay<'a> {
    engine: &'a Engine,
}

impl<'a> DmlCommandMutationOverlay<'a> {
    pub(in crate::sql) fn new(engine: &'a Engine) -> Self {
        engine.begin_command_mutation_overlay();
        Self { engine }
    }
}

impl Drop for DmlCommandMutationOverlay<'_> {
    fn drop(&mut self) {
        self.engine.end_command_mutation_overlay();
    }
}

fn lock_mutation_row(
    engine: &Engine,
    table: &str,
    display_name: &str,
    doc_id: DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Result<bool, SQLError> {
    match engine.lock_row(
        table,
        doc_id,
        strength,
        uqa_sql::ast::LockWait::Block,
        display_name,
    )? {
        crate::row_locks::LockAcquire::Granted { waited, .. } => Ok(waited),
        crate::row_locks::LockAcquire::Skipped => Err(SQLError::Internal(
            "DML row locking used SKIP LOCKED".into(),
        )),
    }
}

/// Outcome of following a DML target through its committed update chain.
pub(in crate::sql) enum MutationLockTarget {
    Present { doc_id: DocId, recheck: bool },
    Deleted,
}

pub(in crate::sql) enum PhysicalMutationLockTarget {
    Present {
        identity: PhysicalDocumentIdentity,
        recheck: bool,
    },
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::sql) struct PhysicalDocumentIdentity {
    pub table: String,
    pub doc_id: DocId,
}

pub(in crate::sql) struct PreparedDocumentRewrite {
    pub table: String,
    pub doc_id: DocId,
    pub destination: Option<(String, DocId)>,
    pub old_document: Document,
    pub new_document: Document,
    pub actions: Vec<PreparedDocumentRewrite>,
    pub trigger_updated_columns: Option<Vec<String>>,
}

pub(in crate::sql) enum PreparedDeleteAction {
    Delete(Box<PreparedDocumentDelete>),
    Rewrite(Box<PreparedDocumentRewrite>),
}

pub(in crate::sql) struct PreparedDocumentDelete {
    pub table: String,
    pub doc_id: DocId,
    pub document: Document,
    pub actions: Vec<PreparedDeleteAction>,
}

#[derive(Default)]
pub(in crate::sql) struct ReferentialActionContext {
    pub(in crate::sql) delete_stack: Vec<(String, DocId)>,
    pub(in crate::sql) rewrite_stack: Vec<(String, DocId)>,
    pub(in crate::sql) trigger_statements: crate::sql::triggers::ReferentialTriggerStatements,
    pending_documents: BTreeMap<PhysicalDocumentIdentity, Option<Document>>,
}

impl ReferentialActionContext {
    pub(in crate::sql) fn pending_document(
        &self,
        identity: &PhysicalDocumentIdentity,
    ) -> Option<&Option<Document>> {
        self.pending_documents.get(identity)
    }

    pub(in crate::sql) fn record_pending_document(
        &mut self,
        identity: PhysicalDocumentIdentity,
        document: Option<Document>,
    ) {
        self.pending_documents.insert(identity, document);
    }

    pub(in crate::sql) fn transition_tables(
        &self,
        engine: &Engine,
        events: &[crate::sql::triggers::AfterRowTriggerEvent],
    ) -> Result<Vec<crate::sql::triggers::TransitionTables>, SQLError> {
        self.trigger_statements
            .build_transition_tables(engine, events)
    }

    pub(in crate::sql) fn fire_after_statement_triggers(
        &self,
        engine: &Engine,
        transitions: &[crate::sql::triggers::TransitionTables],
    ) -> Result<(), SQLError> {
        self.trigger_statements.fire_after(engine, transitions)
    }
}

pub(in crate::sql) struct ReferentialRewritePreparation<'a> {
    pub(in crate::sql) table: &'a str,
    pub(in crate::sql) doc_id: DocId,
    pub(in crate::sql) old_document: Document,
    pub(in crate::sql) proposed_document: Document,
    pub(in crate::sql) updated_columns: Vec<String>,
}

pub(in crate::sql) fn encode_prepared_doc_id(doc_id: DocId) -> Value {
    Value::Bytes(doc_id.to_be_bytes().to_vec())
}

pub(in crate::sql) fn decode_prepared_doc_id(
    value: Value,
    context: &str,
) -> Result<DocId, SQLError> {
    let Value::Bytes(bytes) = value else {
        return Err(SQLError::Internal(format!(
            "{context} has a non-binary document id"
        )));
    };
    let bytes: [u8; std::mem::size_of::<DocId>()] = bytes
        .try_into()
        .map_err(|_| SQLError::Internal(format!("{context} has an invalid document id width")))?;
    Ok(DocId::from_be_bytes(bytes))
}

pub(in crate::sql) fn encode_prepared_document_rewrite(prepared: PreparedDocumentRewrite) -> Value {
    Value::Map(BTreeMap::from([
        ("table".into(), Value::Str(prepared.table)),
        ("doc_id".into(), encode_prepared_doc_id(prepared.doc_id)),
        (
            "destination".into(),
            prepared.destination.map_or(Value::Null, |(table, doc_id)| {
                Value::Map(BTreeMap::from([
                    ("table".into(), Value::Str(table)),
                    ("doc_id".into(), encode_prepared_doc_id(doc_id)),
                ]))
            }),
        ),
        ("old".into(), Value::Map(prepared.old_document)),
        ("new".into(), Value::Map(prepared.new_document)),
        (
            "actions".into(),
            Value::List(
                prepared
                    .actions
                    .into_iter()
                    .map(encode_prepared_document_rewrite)
                    .collect(),
            ),
        ),
    ]))
}

pub(in crate::sql) fn decode_prepared_document_rewrite(
    value: Value,
) -> Result<PreparedDocumentRewrite, SQLError> {
    let Value::Map(mut fields) = value else {
        return Err(SQLError::Internal(
            "prepared rewrite spill payload is not a map".into(),
        ));
    };
    let table = match fields.remove("table") {
        Some(Value::Str(table)) => table,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no table".into(),
            ))
        }
    };
    let doc_id = decode_prepared_doc_id(
        fields.remove("doc_id").ok_or_else(|| {
            SQLError::Internal("prepared rewrite spill payload has no document id".into())
        })?,
        "prepared rewrite spill payload",
    )?;
    let destination = match fields.remove("destination") {
        Some(Value::Null) | None => None,
        Some(Value::Map(mut destination)) => {
            let table = match destination.remove("table") {
                Some(Value::Str(table)) => table,
                _ => {
                    return Err(SQLError::Internal(
                        "prepared rewrite destination has no table".into(),
                    ))
                }
            };
            let doc_id = decode_prepared_doc_id(
                destination.remove("doc_id").ok_or_else(|| {
                    SQLError::Internal("prepared rewrite destination has no document id".into())
                })?,
                "prepared rewrite destination",
            )?;
            Some((table, doc_id))
        }
        Some(_) => {
            return Err(SQLError::Internal(
                "prepared rewrite destination is not a map".into(),
            ))
        }
    };
    let old_document = match fields.remove("old") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no old document".into(),
            ))
        }
    };
    let new_document = match fields.remove("new") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no new document".into(),
            ))
        }
    };
    let actions = match fields.remove("actions") {
        Some(Value::List(actions)) => actions
            .into_iter()
            .map(decode_prepared_document_rewrite)
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no action list".into(),
            ))
        }
    };
    Ok(PreparedDocumentRewrite {
        table,
        doc_id,
        destination,
        old_document,
        new_document,
        actions,
        trigger_updated_columns: None,
    })
}

pub(in crate::sql) fn encode_prepared_document_delete(prepared: PreparedDocumentDelete) -> Value {
    let actions = prepared
        .actions
        .into_iter()
        .map(|action| match action {
            PreparedDeleteAction::Delete(delete) => Value::Map(BTreeMap::from([
                ("kind".into(), Value::Str("delete".into())),
                ("plan".into(), encode_prepared_document_delete(*delete)),
            ])),
            PreparedDeleteAction::Rewrite(rewrite) => Value::Map(BTreeMap::from([
                ("kind".into(), Value::Str("rewrite".into())),
                ("plan".into(), encode_prepared_document_rewrite(*rewrite)),
            ])),
        })
        .collect();
    Value::Map(BTreeMap::from([
        ("table".into(), Value::Str(prepared.table)),
        ("doc_id".into(), encode_prepared_doc_id(prepared.doc_id)),
        ("document".into(), Value::Map(prepared.document)),
        ("actions".into(), Value::List(actions)),
    ]))
}

pub(in crate::sql) fn decode_prepared_document_delete(
    value: Value,
) -> Result<PreparedDocumentDelete, SQLError> {
    let Value::Map(mut fields) = value else {
        return Err(SQLError::Internal(
            "prepared delete spill payload is not a map".into(),
        ));
    };
    let table = match fields.remove("table") {
        Some(Value::Str(table)) => table,
        _ => {
            return Err(SQLError::Internal(
                "prepared delete spill payload has no table".into(),
            ))
        }
    };
    let doc_id = decode_prepared_doc_id(
        fields.remove("doc_id").ok_or_else(|| {
            SQLError::Internal("prepared delete spill payload has no document id".into())
        })?,
        "prepared delete spill payload",
    )?;
    let document = match fields.remove("document") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "prepared delete spill payload has no document".into(),
            ))
        }
    };
    let action_values = match fields.remove("actions") {
        Some(Value::List(actions)) => actions,
        _ => {
            return Err(SQLError::Internal(
                "prepared delete spill payload has no action list".into(),
            ))
        }
    };
    let mut actions = Vec::with_capacity(action_values.len());
    for action in action_values {
        let Value::Map(mut action) = action else {
            return Err(SQLError::Internal(
                "prepared delete action spill payload is not a map".into(),
            ));
        };
        let kind = match action.remove("kind") {
            Some(Value::Str(kind)) => kind,
            _ => {
                return Err(SQLError::Internal(
                    "prepared delete action spill payload has no kind".into(),
                ))
            }
        };
        let plan = action.remove("plan").ok_or_else(|| {
            SQLError::Internal("prepared delete action spill payload has no plan".into())
        })?;
        actions.push(match kind.as_str() {
            "delete" => {
                PreparedDeleteAction::Delete(Box::new(decode_prepared_document_delete(plan)?))
            }
            "rewrite" => {
                PreparedDeleteAction::Rewrite(Box::new(decode_prepared_document_rewrite(plan)?))
            }
            _ => {
                return Err(SQLError::Internal(format!(
                    "prepared delete action spill payload has unknown kind `{kind}`"
                )))
            }
        });
    }
    Ok(PreparedDocumentDelete {
        table,
        doc_id,
        document,
        actions,
    })
}

/// Lock a DML target row and follow any primary-key rewrite another transaction committed while this statement waited, exactly like `PostgreSQL` 18 follows the update chain to the row version it lands on. Returns the doc id the statement must act on together with whether any wait or successor hop makes a re-qualification necessary. Callers acquire every row dependency first and promote the backend writer only after that lock phase has completed.
pub(in crate::sql) fn lock_mutation_target(
    engine: &Engine,
    table: &str,
    display_name: &str,
    doc_id: DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Result<MutationLockTarget, SQLError> {
    let mut current = doc_id;
    let mut recheck = false;
    let mut hops = 0usize;
    loop {
        recheck |= lock_mutation_row(engine, table, display_name, current, strength)?;
        let successor = match engine.committed_row_successor(table, current)? {
            crate::row_locks::RowChangeTarget::Unchanged => {
                return Ok(MutationLockTarget::Present {
                    doc_id: current,
                    recheck,
                });
            }
            crate::row_locks::RowChangeTarget::Deleted
                if engine.current_transaction_uses_fixed_snapshot() =>
            {
                return Err(concurrent_update_serialization_failure());
            }
            crate::row_locks::RowChangeTarget::Deleted => {
                return Ok(MutationLockTarget::Deleted);
            }
            crate::row_locks::RowChangeTarget::Present(_)
                if engine.current_transaction_uses_fixed_snapshot() =>
            {
                return Err(concurrent_update_serialization_failure());
            }
            crate::row_locks::RowChangeTarget::Present(successor) => successor,
        };
        if successor == current {
            return Ok(MutationLockTarget::Present {
                doc_id: current,
                recheck: true,
            });
        }
        hops += 1;
        if hops > 64 {
            return Err(SQLError::Internal(format!(
                "primary-key rewrite chain for `{table}` row {doc_id} did not converge"
            )));
        }
        recheck = true;
        current = successor;
    }
}

/// Lock a DML candidate and follow a committed update chain across physical relations. Declarative partition movement changes the leaf table as well as the document id, so callers that scan a hierarchy must retain the complete successor identity.
pub(in crate::sql) fn lock_physical_mutation_target(
    engine: &Engine,
    table: &str,
    display_name: &str,
    doc_id: DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Result<PhysicalMutationLockTarget, SQLError> {
    let mut current = PhysicalDocumentIdentity {
        table: table.to_string(),
        doc_id,
    };
    let mut recheck = false;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return Err(SQLError::Internal(format!(
                "physical rewrite chain for `{display_name}` row {table}:{doc_id} contains a cycle at {}:{}",
                current.table, current.doc_id
            )));
        }
        recheck |= lock_mutation_row(
            engine,
            &current.table,
            display_name,
            current.doc_id,
            strength,
        )?;
        match engine.committed_physical_row_successor(&current.table, current.doc_id)? {
            crate::row_locks::PhysicalRowChangeTarget::Unchanged => {
                return Ok(PhysicalMutationLockTarget::Present {
                    identity: current,
                    recheck,
                });
            }
            crate::row_locks::PhysicalRowChangeTarget::Deleted
            | crate::row_locks::PhysicalRowChangeTarget::Present { .. }
                if engine.current_transaction_uses_fixed_snapshot() =>
            {
                return Err(concurrent_update_serialization_failure());
            }
            crate::row_locks::PhysicalRowChangeTarget::Deleted => {
                return Ok(PhysicalMutationLockTarget::Deleted);
            }
            crate::row_locks::PhysicalRowChangeTarget::Present { table_hash, doc_id } => {
                let table = engine.row_lock_table_for_hash(table_hash)?;
                if table == current.table && doc_id == current.doc_id {
                    return Ok(PhysicalMutationLockTarget::Present {
                        identity: current,
                        recheck: true,
                    });
                }
                recheck = true;
                current = PhysicalDocumentIdentity { table, doc_id };
            }
        }
    }
}

pub(crate) fn update_lock_strength(
    engine: &Engine,
    table: &str,
    columns: &[String],
) -> uqa_sql::ast::LockStrength {
    let Ok(keys) = engine.try_key_constraints(table) else {
        return uqa_sql::ast::LockStrength::ForUpdate;
    };
    let Ok(Some(definitions)) = engine.try_describe_table(table) else {
        return uqa_sql::ast::LockStrength::ForUpdate;
    };
    let assigned = columns.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let touches_key = keys.iter().any(|constraint| {
        constraint.columns.iter().any(|column| {
            if assigned.contains(column.as_str()) {
                return true;
            }
            let Some(generated) = definitions
                .iter()
                .find(|definition| definition.name == *column)
                .and_then(|definition| definition.generated.as_ref())
            else {
                return false;
            };
            let mut dependencies = BTreeSet::new();
            let expression = uqa_planner::ExpressionPlan::lower((*generated.expression).clone());
            !expression.scalar.collect_columns(&mut dependencies)
                || dependencies
                    .iter()
                    .any(|dependency| assigned.contains(dependency.as_str()))
        })
    });
    if touches_key {
        uqa_sql::ast::LockStrength::ForUpdate
    } else {
        uqa_sql::ast::LockStrength::ForNoKeyUpdate
    }
}

fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}

fn is_virtual_document_id_column(column: &str, definitions: &[uqa_sql::ast::ColumnDef]) -> bool {
    column == DOC_ID_COLUMN
        && !definitions
            .iter()
            .any(|definition| definition.name == DOC_ID_COLUMN)
}

pub(crate) fn stamp_tuple_xmin(
    engine: &Engine,
    table: &str,
    document: &mut Document,
) -> Result<(), SQLError> {
    let xmin = Value::Int(i64::from(engine.tuple_version_xid()?));
    let previous_system_xmin = document.get(XMIN_STORAGE_COLUMN).cloned();
    let schemaless_user_xmin_marked = document
        .get(XMIN_USER_STORAGE_COLUMN)
        .is_some_and(|value| value == &Value::Bool(true));
    let public_xmin_was_system_mirror = previous_system_xmin
        .as_ref()
        .is_some_and(|previous| document.get(XMIN_COLUMN) == Some(previous));
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("resolve tuple-version schema", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let has_declared_xmin = definitions
        .iter()
        .any(|definition| definition.name == XMIN_COLUMN);
    let schemaless_user_xmin = definitions.is_empty()
        && (schemaless_user_xmin_marked
            || (document.contains_key(XMIN_COLUMN)
                && (previous_system_xmin.is_none() || !public_xmin_was_system_mirror)));
    document.insert(XMIN_STORAGE_COLUMN.into(), xmin.clone());
    if schemaless_user_xmin {
        document.insert(XMIN_USER_STORAGE_COLUMN.into(), Value::Bool(true));
    } else {
        document.remove(XMIN_USER_STORAGE_COLUMN);
    }
    if !has_declared_xmin && !schemaless_user_xmin {
        // Keep the legacy projection mirror while old database rows are migrated lazily. The collision-free key above is authoritative and the mirror is never written when `xmin` belongs to the user schema.
        document.insert(XMIN_COLUMN.into(), xmin);
    }
    Ok(())
}

fn dml_target_row(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut materialized = document.clone();
    crate::engine_generated::materialize_virtual_generated_columns(
        &definitions,
        &mut materialized,
    )?;
    let mut columns = if definitions.is_empty() {
        materialized
            .keys()
            .filter(|column| {
                column.as_str() != XMIN_STORAGE_COLUMN
                    && column.as_str() != XMIN_USER_STORAGE_COLUMN
                    && column.as_str() != XMIN_COLUMN
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    columns.push(XMIN_COLUMN.into());
    types.push(Some(ColumnType::Xid));
    let values = columns
        .iter()
        .map(|column| {
            if is_virtual_document_id_column(column, &definitions)
                || definitions.iter().any(|definition| {
                    definition.name == *column
                        && definition.primary_key
                        && definition.ty.is_integer()
                })
            {
                doc_id_value(doc_id)
            } else if column == XMIN_COLUMN {
                Ok(materialized
                    .get(XMIN_STORAGE_COLUMN)
                    .cloned()
                    .unwrap_or(Value::Null))
            } else {
                Ok(materialized.get(column).cloned().unwrap_or(Value::Null))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::from_values(values),
    ))
}

fn dml_null_target_row(
    engine: &Engine,
    table: &str,
    qualifier: &str,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut columns = if definitions.is_empty() {
        engine
            .try_table_columns(table)
            .map_err(|error| dml_storage_error("DML row schema lookup", error))?
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    columns.push(XMIN_COLUMN.into());
    types.push(Some(ColumnType::Xid));
    let width = columns.len();
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::nulls(width),
    ))
}

fn dml_join_rows(left: &OwnedPhysicalRow, right: &OwnedPhysicalRow) -> OwnedPhysicalRow {
    OwnedPhysicalRow::new(
        RowSchema::join(&left.schema, &right.schema, std::iter::empty()),
        PhysicalRow::concat(&left.row, &right.row),
    )
}

fn validate_dml_expression_qualifiers(
    expression: &ScalarExpr,
    allowed: &BTreeSet<String>,
) -> Result<(), SQLError> {
    for qualifier in crate::sql::select::expr_qualifiers(expression) {
        if !allowed.contains(&qualifier) {
            return Err(SQLError::UnknownTable(qualifier));
        }
    }
    Ok(())
}

fn dml_append_hidden_qualified_row(
    base: &OwnedPhysicalRow,
    qualifier: &str,
    columns: &[String],
    types: &[Option<ColumnType>],
    values: Vec<Value>,
) -> OwnedPhysicalRow {
    let hidden_types = columns
        .iter()
        .enumerate()
        .map(|(position, _)| types.get(position).cloned().flatten())
        .collect::<Vec<_>>();
    let schema = RowSchema::append_hidden_typed(&base.schema, &hidden_types);
    let offset = base.schema.physical_width();
    let aliases = columns
        .iter()
        .enumerate()
        .map(|(position, column)| {
            (
                ColumnIdentity::qualified(qualifier, column),
                offset + position,
                hidden_types[position].clone(),
            )
        })
        .collect::<Vec<_>>();
    OwnedPhysicalRow::new(
        RowSchema::with_physical_identity_aliases(&schema, &aliases),
        base.row.clone().append_values(values),
    )
}

fn insert_identity_columns(
    engine: &Engine,
    table: &str,
    action: &str,
) -> Result<(Option<String>, String), SQLError> {
    let auto_increment = engine
        .auto_increment_column(table)
        .map_err(|err| dml_storage_error(action, err))?;
    let id_column = if auto_increment.is_some() {
        auto_increment.clone()
    } else {
        engine
            .try_describe_table(table)
            .map_err(|err| dml_storage_error(action, err))?
            .and_then(|columns| columns.into_iter().find(|column| column.primary_key))
            .map(|column| column.name)
    }
    .unwrap_or_else(|| "id".into());
    Ok((auto_increment, id_column))
}

fn prepare_auto_increment_identity(
    engine: &Engine,
    table: &str,
    id_column: &str,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<Option<(DocId, bool)>, SQLError> {
    let Some(auto_id_column) = auto_id_column else {
        return Ok(None);
    };
    let definitions = engine
        .auto_increment_columns(table)
        .map_err(|error| dml_storage_error(action, error))?;
    let mut selected_generated = false;
    for (column, provenance) in &definitions {
        if !provenance.is_identity() {
            continue;
        }
        let supplied = document
            .get(column)
            .is_some_and(|value| !matches!(value, Value::Null));
        if supplied {
            if provenance.kind == uqa_sql::ast::AutoIncrementKind::IdentityAlways {
                return Err(SQLError::Routine {
                    sqlstate: "428C9".into(),
                    message: format!(
                        "cannot insert a non-DEFAULT value into identity column \"{column}\""
                    ),
                });
            }
            continue;
        }
        let sequence = provenance.sequence.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "identity column `{table}.{column}` has no durable sequence binding"
            ))
        })?;
        let value = engine.nextval_sql(sequence)?;
        document.insert(
            column.clone(),
            crate::sql::coerce_to_column_type(engine, table, column, Value::Int(value))?,
        );
        selected_generated |= column == auto_id_column;
    }
    let provenance = definitions
        .iter()
        .find(|(column, _)| column == auto_id_column)
        .map(|(_, provenance)| provenance)
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "auto-increment column `{table}.{auto_id_column}` disappeared"
            ))
        })?;
    match provenance.kind {
        uqa_sql::ast::AutoIncrementKind::Serial => Ok(None),
        uqa_sql::ast::AutoIncrementKind::IdentityAlways
        | uqa_sql::ast::AutoIncrementKind::IdentityByDefault => {
            let mut identity = prepare_insert_identity(
                engine,
                table,
                id_column,
                Some(auto_id_column),
                document,
                action,
            )?;
            if selected_generated {
                identity.1 = false;
            }
            Ok(Some(identity))
        }
        uqa_sql::ast::AutoIncrementKind::Legacy => {
            let owner = engine.partition_identity_owner(table)?;
            engine.lock_relation(&owner, crate::row_locks::RelationLockMode::RowExclusive)?;
            prepare_insert_identity(
                engine,
                &owner,
                id_column,
                Some(auto_id_column),
                document,
                action,
            )
            .map(Some)
        }
    }
}

fn persist_auto_increment_identity(
    engine: &Engine,
    table: &str,
    auto_id_column: Option<&str>,
    action: &str,
) -> Result<(), SQLError> {
    let Some(auto_id_column) = auto_id_column else {
        return Ok(());
    };
    let legacy = engine
        .auto_increment_columns(table)
        .map_err(|error| dml_storage_error(action, error))?
        .into_iter()
        .find(|(column, _)| column == auto_id_column)
        .is_some_and(|(_, provenance)| provenance.kind == uqa_sql::ast::AutoIncrementKind::Legacy);
    if !legacy {
        return Ok(());
    }
    let owner = engine.partition_identity_owner(table)?;
    engine
        .persist_next_id(&owner)
        .map_err(|error| dml_storage_error(action, error))
}

fn prepare_insert_identity(
    engine: &Engine,
    allocation_table: &str,
    id_column: &str,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<(DocId, bool), SQLError> {
    let supplied_id = document_supplied_id(document, id_column, auto_id_column == Some(id_column))?;
    let supplied = supplied_id.is_some();
    let doc_id = match supplied_id {
        Some(doc_id) => doc_id,
        None => engine.allocate_next_id(allocation_table)?,
    };
    if auto_id_column == Some(id_column) {
        document.insert(id_column.to_string(), doc_id_value(doc_id)?);
    }
    engine
        .advance_next_id(allocation_table, doc_id)
        .map_err(|error| dml_storage_error(action, error))?;
    Ok((doc_id, supplied))
}

fn validate_mutation_columns<'a>(
    engine: &Engine,
    table: &str,
    columns: impl IntoIterator<Item = &'a str>,
    action: &str,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error(action, err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    // Programmatically-created document tables intentionally have no SQL
    // schema and retain their open-field behavior. SQL CREATE TABLE always
    // supplies definitions, and those targets must reject misspelled or
    // repeated mutation columns instead of persisting arbitrary fields.
    if definitions.is_empty() {
        return Ok(());
    }
    let known: BTreeSet<&str> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(column) {
            return Err(SQLError::TypeMismatch(format!(
                "{action}: column `{column}` is specified more than once"
            )));
        }
        if !known.contains(column) {
            return Err(SQLError::UnknownColumn(format!("{table}.{column}")));
        }
    }
    Ok(())
}

fn eval_mutation_expr(
    engine: &Engine,
    ctes: &CteScope,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    let empty_schema = RowSchema::default();
    let schema = row.map_or(&empty_schema, |row| &row.schema);
    uqa_execution::scalar_type_with_resolver(expression, schema, params, engine)?;
    let expression = uqa_execution::bind_type_introspection_with_resolver(
        expression.clone(),
        schema,
        params,
        engine,
    );
    if let Some(row) = row {
        let view = row.view();
        let context = PhysicalEvalContext::from_row_lookup(&view, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook)
            .with_physical_outer_row(&row.schema, &row.row);
        eval_physical_scalar(&expression, &ctes.scalar_subqueries, &context)
    } else {
        let context = PhysicalEvalContext::new(None, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        eval_physical_scalar(&expression, &ctes.scalar_subqueries, &context)
    }
}

struct MutationAssignmentTarget<'a> {
    table: &'a str,
    column: &'a str,
    action: &'a str,
}

fn eval_mutation_assignment(
    engine: &Engine,
    ctes: &CteScope,
    target: MutationAssignmentTarget<'_>,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let MutationAssignmentTarget {
        table,
        column,
        action,
    } = target;
    let generated = crate::sql::generated::generated_column_kind(engine, table, column)?;
    if matches!(expression, ScalarExpr::Default) {
        if generated.is_some() {
            return Ok(None);
        }
        let value = match engine
            .try_column_default_expr(table, column)
            .map_err(|error| dml_storage_error(action, error))?
        {
            Some(default) => eval_lowered_expression(engine, &default, None, params)?,
            None => Value::Null,
        };
        return coerce_to_column_type(engine, table, column, value).map(Some);
    }
    if generated.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "column `{column}` is a generated column; only DEFAULT may be assigned"
        )));
    }
    let value = eval_mutation_expr(engine, ctes, expression, row, params)?;
    coerce_to_column_type(engine, table, column, value).map(Some)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergePairKind {
    Matched,
    NotMatchedBySource,
    NotMatchedByTarget,
}

impl MergePairKind {
    fn encode(self) -> i64 {
        match self {
            Self::Matched => 0,
            Self::NotMatchedBySource => 1,
            Self::NotMatchedByTarget => 2,
        }
    }

    fn decode(value: &Value) -> Result<Self, SQLError> {
        match value {
            Value::Int(0) => Ok(Self::Matched),
            Value::Int(1) => Ok(Self::NotMatchedBySource),
            Value::Int(2) => Ok(Self::NotMatchedByTarget),
            other => Err(SQLError::Internal(format!(
                "invalid spilled MERGE pairing kind {other:?}"
            ))),
        }
    }
}

struct MergePairing {
    kind: MergePairKind,
    storage_table: Option<String>,
    doc_id: Option<DocId>,
    target_document: Option<Document>,
    source_row: OwnedPhysicalRow,
}

fn merge_pair_schema(source: &RowSchema) -> RowSchema {
    let header = RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![
            Some(ColumnType::BigInteger),
            Some(ColumnType::Text),
            Some(ColumnType::Text),
            None,
        ],
    );
    RowSchema::join(&header, source, std::iter::empty())
}

fn encode_merge_pair(
    kind: MergePairKind,
    storage_table: Option<&str>,
    doc_id: Option<DocId>,
    target_document: Option<&Document>,
    source_row: &OwnedPhysicalRow,
) -> uqa_execution::PhysicalRow {
    let header = PhysicalRow::from_values(vec![
        Value::Int(kind.encode()),
        storage_table.map_or(Value::Null, |table| Value::Str(table.to_string())),
        doc_id.map_or(Value::Null, |doc_id| Value::Str(doc_id.to_string())),
        target_document.map_or(Value::Null, |document| Value::Map(document.clone())),
    ]);
    PhysicalRow::concat(&header, &source_row.row)
}

fn decode_merge_pair(encoded: OwnedPhysicalRow) -> Result<MergePairing, SQLError> {
    let kind =
        MergePairKind::decode(encoded.physical_value_at(0).ok_or_else(|| {
            SQLError::Internal("spilled MERGE pairing lost its match kind".into())
        })?)?;
    let storage_table = match encoded.physical_value_at(1) {
        Some(Value::Str(table)) => Some(table.clone()),
        Some(Value::Null) | None => None,
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE storage table value {value:?}"
            )))
        }
    };
    let doc_id = match encoded.physical_value_at(2) {
        Some(Value::Null) | None => None,
        Some(Value::Str(doc_id)) => Some(doc_id.parse::<DocId>().map_err(|error| {
            SQLError::Internal(format!(
                "invalid spilled MERGE document id `{doc_id}`: {error}"
            ))
        })?),
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE document id value {value:?}"
            )))
        }
    };
    let target_document = match encoded.physical_value_at(3) {
        Some(Value::Map(document)) => Some(document.clone()),
        Some(Value::Null) | None => None,
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE target document value {value:?}"
            )))
        }
    };
    match kind {
        MergePairKind::Matched | MergePairKind::NotMatchedBySource
            if storage_table.is_none() || doc_id.is_none() || target_document.is_none() =>
        {
            return Err(SQLError::Internal(
                "target-bearing MERGE pairing lost its target row".into(),
            ));
        }
        MergePairKind::NotMatchedByTarget
            if storage_table.is_some() || doc_id.is_some() || target_document.is_some() =>
        {
            return Err(SQLError::Internal(
                "target-missing MERGE pairing unexpectedly retained a target row".into(),
            ));
        }
        _ => {}
    }
    Ok(MergePairing {
        kind,
        storage_table,
        doc_id,
        target_document,
        source_row: encoded,
    })
}

fn merge_source_index_value(index: usize) -> Value {
    Value::Str(index.to_string())
}

mod conflict;
mod constraints;
mod delete;
mod insert;
mod merge;
mod update;
mod update_from;
mod vectors;

pub(in crate::sql) use conflict::*;
pub(crate) use constraints::*;
pub(in crate::sql) use delete::*;
pub(in crate::sql) use insert::*;
pub(in crate::sql) use merge::*;
pub(in crate::sql) use update::*;
pub(in crate::sql) use update_from::*;
pub(in crate::sql) use vectors::*;
