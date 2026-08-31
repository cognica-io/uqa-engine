//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT row validation, staging, publication, defaults, and vector extraction.

use std::sync::Arc;

use super::{
    build_returning_row, coerce_to_column_type, dml_storage_error, document_supplied_id,
    eval_lowered_expression, lock_document_key_dependencies,
    lock_existing_document_foreign_key_dependencies, partition_insert_target,
    stage_prepared_document_rewrite, validate_document_non_key_constraints,
    validate_key_constraints, validate_view_checks, CteScope, DocId, Document, Engine,
    InsertConflictLocks, InsertConflictPreparation, InsertPlan, MutationPublicationBatch,
    MutationRowImage, MutationRowImages, PreparedDocumentInsert, PreparedInsertConflict,
    PreparedInsertRowContext, PreparedMutationAction, ReturningProjectionRow, SQLError, SQLParam,
    ViewCheckContext,
};

pub(super) struct StagedValuesInsertRow {
    pub(super) target_table: String,
    pub(super) document: Arc<Document>,
    pub(super) prepared: PreparedInsertConflict,
    pub(super) returning: Option<uqa_execution::OwnedPhysicalRow>,
    pub(super) after_row_events: Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    pub(super) prepared_effect: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_values_insert_row(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
    snapshot_scope: &CteScope,
    conflict_update_columns: &[String],
    auto_id_column: Option<&str>,
    id_column: &str,
    accepts_supplied_identity: bool,
    target_table: String,
    mut document: Document,
    mut insert_identity: (DocId, bool),
    conflict_locks: &mut InsertConflictLocks,
    referential_actions: &mut super::super::ReferentialActionContext,
) -> Result<Option<StagedValuesInsertRow>, SQLError> {
    let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
        engine,
        &target_table,
        uqa_sql::ast::TriggerEvent::Insert,
        insert_identity.0,
        None,
        Some(&document),
        &[],
    )?
    else {
        return Ok(None);
    };
    document = triggered_document;
    crate::sql::generated::refresh_stored_generated_columns(engine, &target_table, &mut document)?;
    refresh_insert_identity_after_trigger(
        engine,
        &target_table,
        id_column,
        accepts_supplied_identity,
        auto_id_column,
        &document,
        &mut insert_identity,
    )?;
    let trigger_target = partition_insert_target(
        engine,
        &stmt.table,
        &document,
        params,
        stmt.include_descendants,
    )?;
    if trigger_target != target_table {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "moving row to another partition during a BEFORE FOR EACH ROW trigger is not supported".into(),
        });
    }
    super::super::stamp_tuple_xmin(engine, &target_table, &mut document)?;
    lock_existing_document_foreign_key_dependencies(engine, &target_table, &document)?;
    let prepared = if let Some(on_conflict) = stmt.on_conflict.as_ref() {
        conflict_locks.prepare_document(
            InsertConflictPreparation {
                engine,
                table: &target_table,
                target_qualifier: &stmt.target_qualifier,
                on_conflict,
                document: &document,
                params,
                scope: snapshot_scope,
            },
            referential_actions,
        )?
    } else {
        let _key_locks = lock_document_key_dependencies(engine, &target_table, &document, None)?;
        PreparedInsertConflict::Unresolved
    };
    let mut prepared = attach_prepared_insert_identity(prepared, insert_identity);
    let prepared_effect = !matches!(&prepared, PreparedInsertConflict::Skip);
    let document = Arc::new(document);
    let (returning, after_row_events) = stage_prepared_insert_row(
        PreparedInsertRowContext {
            engine,
            stmt,
            storage_table: &target_table,
            document: document.as_ref(),
            shared_document: Some(&document),
            conflict_update_columns,
            params,
            scope: snapshot_scope,
        },
        &mut prepared,
    )?;
    Ok(Some(StagedValuesInsertRow {
        target_table,
        document,
        prepared,
        returning,
        after_row_events,
        prepared_effect,
    }))
}

pub(super) fn attach_prepared_insert_identity(
    prepared: PreparedInsertConflict,
    (doc_id, supplied): (DocId, bool),
) -> PreparedInsertConflict {
    match prepared {
        PreparedInsertConflict::Unresolved => PreparedInsertConflict::Insert { doc_id, supplied },
        resolved => resolved,
    }
}

pub(super) fn stage_prepared_insert_row(
    context: PreparedInsertRowContext<'_>,
    prepared: &mut PreparedInsertConflict,
) -> Result<
    (
        Option<uqa_execution::OwnedPhysicalRow>,
        Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    ),
    SQLError,
> {
    let PreparedInsertRowContext {
        engine,
        stmt,
        storage_table,
        document,
        shared_document,
        conflict_update_columns,
        params,
        scope,
    } = context;
    validate_document_non_key_constraints(engine, storage_table, document, params)?;
    let (images, after_row_events) = match prepared {
        PreparedInsertConflict::Insert { doc_id, .. } => {
            validate_key_constraints(engine, storage_table, document, None)?;
            validate_view_checks(ViewCheckContext {
                engine,
                table: &stmt.table,
                storage_table,
                target_qualifier: &stmt.target_qualifier,
                doc_id: *doc_id,
                document,
                checks: &stmt.view_checks,
                params,
                scope,
            })?;
            if let Some(shared_document) = shared_document {
                engine.stage_shared_command_document(
                    storage_table,
                    *doc_id,
                    Some(Arc::clone(shared_document)),
                )?;
            } else {
                engine.stage_command_document(storage_table, *doc_id, Some(document.clone()))?;
            }
            let mut after_row_events = Vec::new();
            if let Some(event) = crate::sql::triggers::AfterRowTriggerEvent::prepare(
                engine,
                crate::sql::triggers::AfterRowTriggerInput {
                    table: storage_table,
                    event: uqa_sql::ast::TriggerEvent::Insert,
                    old_doc_id: *doc_id,
                    new_doc_id: *doc_id,
                    old_document: None,
                    new_document: Some(document),
                    updated_columns: &[],
                    cascade_parent: None,
                },
            )? {
                crate::sql::triggers::AfterRowTriggerEvent::push(&mut after_row_events, event);
            }
            (
                MutationRowImages {
                    old: None,
                    new: Some(MutationRowImage {
                        storage_table: storage_table.to_string(),
                        doc_id: *doc_id,
                        document,
                    }),
                },
                after_row_events,
            )
        }
        PreparedInsertConflict::Updated(prepared) => {
            let old_storage_table = prepared.table.clone();
            let new_storage_table = prepared
                .destination
                .as_ref()
                .map_or_else(|| old_storage_table.clone(), |(table, _)| table.clone());
            let old_doc_id = prepared.doc_id;
            let primary_key_doc_id = super::super::integer_primary_key_doc_id(
                engine,
                &stmt.table,
                &prepared.new_document,
            )?;
            let new_doc_id = prepared
                .destination
                .as_ref()
                .map(|(_, doc_id)| *doc_id)
                .or(primary_key_doc_id)
                .unwrap_or(old_doc_id);
            validate_key_constraints(
                engine,
                &new_storage_table,
                &prepared.new_document,
                (new_storage_table == old_storage_table).then_some(old_doc_id),
            )?;
            validate_view_checks(ViewCheckContext {
                engine,
                table: &stmt.table,
                storage_table: &new_storage_table,
                target_qualifier: &stmt.target_qualifier,
                doc_id: new_doc_id,
                document: &prepared.new_document,
                checks: &stmt.view_checks,
                params,
                scope,
            })?;
            let mut after_row_events = Vec::new();
            let doc_id = stage_prepared_document_rewrite(
                engine,
                prepared,
                params,
                Some(conflict_update_columns),
                &mut after_row_events,
            )?;
            (
                MutationRowImages {
                    old: Some(MutationRowImage {
                        storage_table: old_storage_table,
                        doc_id: old_doc_id,
                        document: &prepared.old_document,
                    }),
                    new: Some(MutationRowImage {
                        storage_table: new_storage_table,
                        doc_id,
                        document: &prepared.new_document,
                    }),
                },
                after_row_events,
            )
        }
        PreparedInsertConflict::Skip => return Ok((None, Vec::new())),
        PreparedInsertConflict::Unresolved => {
            return Err(SQLError::Internal(
                "INSERT command overlay has no prepared document identity".into(),
            ))
        }
    };
    let returning = if stmt.returning.is_empty() {
        None
    } else {
        Some(build_returning_row(
            engine,
            ReturningProjectionRow {
                table: &stmt.table,
                target_qualifier: &stmt.target_qualifier,
                images,
                aliases: &stmt.returning_aliases,
                context: None,
            },
            &stmt.returning,
            params,
            scope,
        )?)
    };
    Ok((returning, after_row_events))
}

pub(super) fn apply_validated_prepared_insert(
    engine: &Engine,
    table: &str,
    document: Document,
    prepared: PreparedInsertConflict,
    known_new: bool,
    publication: &mut MutationPublicationBatch,
) -> Result<bool, SQLError> {
    match prepared {
        PreparedInsertConflict::Skip => Ok(false),
        PreparedInsertConflict::Updated(rewrite) => {
            super::publish_prepared_mutation_action(
                engine,
                PreparedMutationAction::Rewrite(rewrite),
                false,
                publication,
            )?;
            Ok(true)
        }
        PreparedInsertConflict::Insert { doc_id, .. } => {
            super::publish_prepared_mutation_action(
                engine,
                PreparedMutationAction::Insert(PreparedDocumentInsert {
                    table: table.to_string(),
                    doc_id,
                    document,
                }),
                known_new,
                publication,
            )?;
            Ok(true)
        }
        PreparedInsertConflict::Unresolved => Err(SQLError::Internal(
            "INSERT reached execution without a prepared document identity".into(),
        )),
    }
}

pub(in crate::sql) fn refresh_insert_identity_after_trigger(
    engine: &Engine,
    table: &str,
    id_column: &str,
    accepts_supplied_identity: bool,
    auto_id_column: Option<&str>,
    document: &Document,
    identity: &mut (DocId, bool),
) -> Result<(), SQLError> {
    if !accepts_supplied_identity {
        return Ok(());
    }
    let Some(doc_id) =
        document_supplied_id(document, id_column, auto_id_column == Some(id_column))?
    else {
        return Ok(());
    };
    if doc_id == identity.0 {
        return Ok(());
    }
    let owner = engine.partition_identity_owner(table)?;
    engine
        .advance_next_id(&owner, doc_id)
        .map_err(|error| dml_storage_error("apply BEFORE INSERT trigger identity", error))?;
    *identity = (doc_id, true);
    Ok(())
}

pub(in crate::sql) fn apply_missing_column_defaults(
    engine: &Engine,
    table: &str,
    document: &mut Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("INSERT defaults", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    for definition in columns {
        let col = definition.name;
        if document.contains_key(&col) {
            continue;
        }
        if definition
            .auto_increment
            .as_ref()
            .is_some_and(uqa_sql::ast::AutoIncrement::is_identity)
        {
            continue;
        }
        if let Some(default_expr) = engine
            .try_column_default_expr(table, &col)
            .map_err(|err| dml_storage_error("INSERT defaults", err))?
        {
            let value = coerce_to_column_type(
                engine,
                table,
                &col,
                eval_lowered_expression(engine, &default_expr, None, params)?,
            )?;
            document.insert(col, value);
        }
    }
    Ok(())
}
