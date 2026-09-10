//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare and stage INSERT row images before durable publication.
use crate::mutation::preparation::MutationPreparationContext;
use crate::{
    mutation::{
        assignment::{validate_view_checks, ViewCheckContext},
        conflict::update::{InsertConflictLocks, InsertConflictPreparation},
        constraints::{
            lock_document_key_dependencies, lock_existing_document_foreign_key_dependencies,
            validate_document_non_key_constraints, validate_key_constraints,
        },
        events::ReferentialActionContext,
        identity::refresh_insert_identity_after_trigger,
        prepared::PreparedInsertConflict,
        returning::{build_returning_row, ReturningProjectionRow},
        row_images::{MutationRowImage, MutationRowImages},
        staging::stage_prepared_document_rewrite,
    },
    query::CteScope,
};
use std::sync::Arc;
use uqa_core::DocId;
use uqa_sql::{
    plan::InsertPlan, semantics::partition::partition_insert_target, SQLError, SQLParam,
};
use uqa_storage::document_store::Document;
pub struct StagedValuesInsertRow {
    pub target_table: String,
    pub document: Arc<Document>,
    pub prepared: PreparedInsertConflict,
    pub returning: Option<crate::OwnedPhysicalRow>,
    pub after_row_events: Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
    pub prepared_effect: bool,
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
pub fn prepare_values_insert_row<S: Clone + 'static>(
    services: MutationPreparationContext<'_, S>,
    stmt: &InsertPlan,
    params: &[SQLParam],
    snapshot_scope: &CteScope<S>,
    conflict_update_columns: &[String],
    auto_id_column: Option<&str>,
    id_column: &str,
    accepts_supplied_identity: bool,
    target_table: String,
    mut document: Document,
    mut insert_identity: (DocId, bool),
    conflict_locks: &mut InsertConflictLocks,
    referential_actions: &mut ReferentialActionContext,
) -> Result<Option<StagedValuesInsertRow>, SQLError> {
    let Some(triggered_document) = crate::mutation::triggers::fire_before_row_triggers(
        &services.referential.triggers,
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
    crate::mutation::assignment::refresh_stored_generated_columns(
        services.referential.assignment,
        &target_table,
        &mut document,
    )?;
    refresh_insert_identity_after_trigger(
        crate::mutation::identity::IdentityAllocationContext {
            identifiers: services.referential.identifiers,
            partitions: services.referential.constraints.partitions.catalog,
        },
        &target_table,
        id_column,
        accepts_supplied_identity,
        auto_id_column,
        &document,
        &mut insert_identity,
    )?;
    let trigger_target = partition_insert_target(
        &services.referential.constraints.partitions,
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
    lock_existing_document_foreign_key_dependencies(
        services.referential.constraints,
        &target_table,
        &document,
    )?;
    let prepared = if let Some(on_conflict) = stmt.on_conflict.as_ref() {
        conflict_locks.prepare_document(
            InsertConflictPreparation {
                context: services.referential,
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
        let _key_locks = lock_document_key_dependencies(
            services.referential.constraints,
            &target_table,
            &document,
            None,
        )?;
        PreparedInsertConflict::Unresolved
    };
    let mut prepared = attach_prepared_insert_identity(prepared, insert_identity);
    let prepared_effect = !matches!(&prepared, PreparedInsertConflict::Skip);
    let document = Arc::new(document);
    let (returning, after_row_events) = stage_prepared_insert_row(
        PreparedInsertRowContext {
            services,
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

pub fn attach_prepared_insert_identity(
    prepared: PreparedInsertConflict,
    (doc_id, supplied): (DocId, bool),
) -> PreparedInsertConflict {
    match prepared {
        PreparedInsertConflict::Unresolved => PreparedInsertConflict::Insert { doc_id, supplied },
        resolved => resolved,
    }
}

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub fn stage_prepared_insert_row<S: Clone + 'static>(
    context: PreparedInsertRowContext<'_, S>,
    prepared: &mut PreparedInsertConflict,
) -> Result<
    (
        Option<crate::OwnedPhysicalRow>,
        Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
    ),
    SQLError,
> {
    let PreparedInsertRowContext {
        services,
        stmt,
        storage_table,
        document,
        shared_document,
        conflict_update_columns,
        params,
        scope,
    } = context;
    validate_document_non_key_constraints(
        services.referential.constraints,
        storage_table,
        document,
        params,
    )?;
    let (images, after_row_events) = match prepared {
        PreparedInsertConflict::Insert { doc_id, .. } => {
            validate_key_constraints(
                services.referential.constraints,
                storage_table,
                document,
                None,
            )?;
            validate_view_checks(ViewCheckContext {
                services: services.referential.assignment,
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
                services.staging.commands.stage_shared_command_document(
                    storage_table,
                    *doc_id,
                    Some(Arc::clone(shared_document)),
                )?;
            } else {
                services.staging.commands.stage_command_document(
                    storage_table,
                    *doc_id,
                    Some(document.clone()),
                )?;
            }
            let mut after_row_events = Vec::new();
            if let Some(event) = crate::mutation::triggers::AfterRowTriggerEvent::prepare(
                &services.referential.triggers,
                crate::mutation::triggers::AfterRowTriggerInput {
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
                crate::mutation::triggers::AfterRowTriggerEvent::push(&mut after_row_events, event);
            }
            (
                MutationRowImages {
                    old: None,
                    new: Some(MutationRowImage {
                        storage_table: storage_table.to_string(),
                        doc_id: *doc_id,
                        document,
                        metadata: crate::mutation::rows::new_tuple_metadata(
                            services.referential.assignment.rows,
                        )?,
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
            let primary_key_doc_id = crate::mutation::identity::integer_primary_key_doc_id(
                services.referential.constraints.catalog,
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
                services.referential.constraints,
                &new_storage_table,
                &prepared.new_document,
                (new_storage_table == old_storage_table).then_some(old_doc_id),
            )?;
            validate_view_checks(ViewCheckContext {
                services: services.referential.assignment,
                table: &stmt.table,
                storage_table: &new_storage_table,
                target_qualifier: &stmt.target_qualifier,
                doc_id: new_doc_id,
                document: &prepared.new_document,
                checks: &stmt.view_checks,
                params,
                scope,
            })?;
            let old_metadata = crate::mutation::rows::existing_tuple_metadata(
                services.referential.assignment.rows,
                &old_storage_table,
                old_doc_id,
            )?;
            let new_metadata =
                crate::mutation::rows::new_tuple_metadata(services.referential.assignment.rows)?;
            let mut after_row_events = Vec::new();
            let doc_id = stage_prepared_document_rewrite(
                services.staging,
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
                        metadata: old_metadata,
                    }),
                    new: Some(MutationRowImage {
                        storage_table: new_storage_table,
                        doc_id,
                        document: &prepared.new_document,
                        metadata: new_metadata,
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
            services.returning,
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

pub struct PreparedInsertRowContext<'a, S: Clone + 'static> {
    pub services: MutationPreparationContext<'a, S>,
    pub stmt: &'a InsertPlan,
    pub storage_table: &'a str,
    pub document: &'a Document,
    pub shared_document: Option<&'a Arc<Document>>,
    pub conflict_update_columns: &'a [String],
    pub params: &'a [SQLParam],
    pub scope: &'a CteScope<S>,
}
