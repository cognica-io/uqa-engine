//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare UPDATE row images, routing, transition events, and RETURNING before publication.
use super::{
    assignment::{validate_view_checks, ViewCheckContext},
    constraints::validate_key_constraints,
    events::ReferentialActionContext,
    identity::integer_primary_key_doc_id,
    preparation::MutationPreparationContext,
    prepared::PreparedDocumentRewrite,
    referential::{prepare_partition_update_route, prepare_routed_document_rewrite},
    returning::{build_returning_row, ReturningProjectionRow},
    row_images::{MutationRowImage, MutationRowImages},
    rows::{existing_tuple_metadata, new_tuple_metadata},
    staging::stage_prepared_document_rewrite,
    triggers::{fire_before_row_triggers, AfterRowTriggerEvent},
};
use crate::query::CteScope;
use uqa_sql::{plan::UpdatePlan, SQLError, SQLParam};
use uqa_storage::document_store::Document;

pub struct PreparedUpdateRow {
    pub rewrite: PreparedDocumentRewrite,
    pub after_row_events: Vec<AfterRowTriggerEvent>,
    pub returning: Option<crate::OwnedPhysicalRow>,
    pub affected: bool,
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub fn prepare_update_row<S: Clone + 'static>(
    context: MutationPreparationContext<'_, S>,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    snapshot_ctes: &CteScope<S>,
    assigned_columns: &[String],
    storage_table: &str,
    doc_id: uqa_core::DocId,
    original_document: Document,
    document: Document,
    referential_actions: &mut ReferentialActionContext,
) -> Result<Option<PreparedUpdateRow>, SQLError> {
    let Some(triggered_document) = fire_before_row_triggers(
        &context.referential.triggers,
        storage_table,
        uqa_sql::ast::TriggerEvent::Update,
        doc_id,
        Some(&original_document),
        Some(&document),
        assigned_columns,
    )?
    else {
        return Ok(None);
    };
    let Some(route) = prepare_partition_update_route(
        &context.referential,
        storage_table,
        doc_id,
        &original_document,
        triggered_document,
        &stmt.table,
        params,
        stmt.include_descendants,
    )?
    else {
        return Ok(None);
    };
    let Some(mut rewrite) = prepare_routed_document_rewrite(
        &context.referential,
        storage_table,
        doc_id,
        original_document,
        route,
        params,
        referential_actions,
    )?
    else {
        return Ok(None);
    };
    let primary_key_doc_id = integer_primary_key_doc_id(
        context.referential.constraints.catalog,
        &stmt.table,
        &rewrite.new_document,
    )?;
    let rewritten_doc_id = rewrite
        .destination
        .as_ref()
        .map(|(_, doc_id)| *doc_id)
        .or(primary_key_doc_id)
        .unwrap_or(rewrite.doc_id);
    let rewritten_storage_table = rewrite
        .destination
        .as_ref()
        .map_or_else(|| rewrite.table.clone(), |(table, _)| table.clone());
    validate_key_constraints(
        context.referential.constraints,
        &rewritten_storage_table,
        &rewrite.new_document,
        (rewritten_storage_table == rewrite.table).then_some(rewrite.doc_id),
    )?;
    validate_view_checks(ViewCheckContext {
        services: context.referential.assignment,
        table: &stmt.table,
        storage_table: &rewritten_storage_table,
        target_qualifier: &stmt.target_qualifier,
        doc_id: rewritten_doc_id,
        document: &rewrite.new_document,
        checks: &stmt.view_checks,
        params,
        scope: snapshot_ctes,
    })?;
    let affected = !rewrite.is_partition_move_delete();
    let old_metadata = existing_tuple_metadata(
        context.referential.assignment.rows,
        &rewrite.table,
        rewrite.doc_id,
    )?;
    let new_metadata = new_tuple_metadata(context.referential.assignment.rows)?;
    let mut after_row_events = Vec::new();
    let rewritten_doc_id = stage_prepared_document_rewrite(
        context.staging,
        &mut rewrite,
        params,
        Some(assigned_columns),
        &mut after_row_events,
    )?;
    let returning = if !affected || stmt.returning.is_empty() {
        None
    } else {
        Some(build_returning_row(
            context.returning,
            ReturningProjectionRow {
                table: &stmt.table,
                target_qualifier: &stmt.target_qualifier,
                images: MutationRowImages {
                    old: Some(MutationRowImage {
                        storage_table: rewrite.table.clone(),
                        doc_id: rewrite.doc_id,
                        document: &rewrite.old_document,
                        metadata: old_metadata,
                    }),
                    new: Some(MutationRowImage {
                        storage_table: rewritten_storage_table,
                        doc_id: rewritten_doc_id,
                        document: &rewrite.new_document,
                        metadata: new_metadata,
                    }),
                },
                aliases: &stmt.returning_aliases,
                context: None,
            },
            &stmt.returning,
            params,
            snapshot_ctes,
        )?)
    };
    Ok(Some(PreparedUpdateRow {
        rewrite,
        after_row_events,
        returning,
        affected,
    }))
}

pub mod from;

pub mod table;
