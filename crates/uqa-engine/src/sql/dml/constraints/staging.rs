//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared document rewrite staging and trigger-event capture.

use super::super::stage_prepared_document_delete_with_parent;
use super::{
    integer_primary_key_doc_id, validate_document_non_key_constraints,
    validate_document_rewrite_constraints, validate_key_constraints, DocId, Engine,
    PreparedDocumentRewrite, SQLError, SQLParam,
};

pub(in crate::sql) fn stage_prepared_document_rewrite(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
    params: &[SQLParam],
    root_updated_columns: Option<&[String]>,
    after_row_events: &mut Vec<crate::sql::triggers::AfterRowTriggerEvent>,
) -> Result<DocId, SQLError> {
    stage_prepared_document_rewrite_with_parent(
        engine,
        prepared,
        params,
        root_updated_columns,
        after_row_events,
        None,
    )
}

pub(in crate::sql) fn stage_prepared_document_rewrite_with_parent(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
    params: &[SQLParam],
    root_updated_columns: Option<&[String]>,
    after_row_events: &mut Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    mut cascade_parent: Option<usize>,
) -> Result<DocId, SQLError> {
    let trigger_updated_columns = root_updated_columns
        .map(<[String]>::to_vec)
        .or_else(|| prepared.trigger_updated_columns.clone());
    if let Some(delete) = prepared.partition_move_delete.as_mut() {
        stage_prepared_document_delete_with_parent(
            engine,
            delete,
            params,
            after_row_events,
            cascade_parent,
        )?;
        if prepared.capture_partition_move_update_transition {
            if let Some(updated_columns) = trigger_updated_columns.as_deref() {
                if let Some(event) =
                    crate::sql::triggers::AfterRowTriggerEvent::prepare_transition_capture(
                        engine,
                        crate::sql::triggers::AfterRowTriggerInput {
                            table: &prepared.table,
                            event: uqa_sql::ast::TriggerEvent::Update,
                            old_doc_id: prepared.doc_id,
                            new_doc_id: prepared.doc_id,
                            old_document: Some(&prepared.old_document),
                            new_document: None,
                            updated_columns,
                            cascade_parent,
                        },
                    )?
                {
                    crate::sql::triggers::AfterRowTriggerEvent::push(after_row_events, event);
                }
            }
        }
        return Ok(prepared.doc_id);
    }
    let rewritten_doc_id =
        if let Some((destination_table, destination_doc_id)) = prepared.destination.as_ref() {
            validate_document_non_key_constraints(
                engine,
                destination_table,
                &prepared.new_document,
                params,
            )?;
            validate_key_constraints(engine, destination_table, &prepared.new_document, None)?;
            engine.stage_command_document(&prepared.table, prepared.doc_id, None)?;
            engine.stage_command_document(
                destination_table,
                *destination_doc_id,
                Some(prepared.new_document.clone()),
            )?;
            *destination_doc_id
        } else {
            validate_document_rewrite_constraints(
                engine,
                &prepared.table,
                &prepared.old_document,
                &prepared.new_document,
                params,
                prepared.doc_id,
            )?;
            let rewritten_doc_id =
                integer_primary_key_doc_id(engine, &prepared.table, &prepared.new_document)?
                    .unwrap_or(prepared.doc_id);
            if rewritten_doc_id != prepared.doc_id {
                engine.stage_command_document(&prepared.table, prepared.doc_id, None)?;
            }
            engine.stage_command_document(
                &prepared.table,
                rewritten_doc_id,
                Some(prepared.new_document.clone()),
            )?;
            rewritten_doc_id
        };
    if let Some((destination_table, _)) = prepared.destination.as_ref() {
        let movement_parent = cascade_parent;
        let mut last_movement_event = None;
        if let Some(event) = crate::sql::triggers::AfterRowTriggerEvent::prepare(
            engine,
            crate::sql::triggers::AfterRowTriggerInput {
                table: &prepared.table,
                event: uqa_sql::ast::TriggerEvent::Delete,
                old_doc_id: prepared.doc_id,
                new_doc_id: prepared.doc_id,
                old_document: Some(&prepared.old_document),
                new_document: None,
                updated_columns: &[],
                cascade_parent: movement_parent,
            },
        )? {
            last_movement_event = Some(crate::sql::triggers::AfterRowTriggerEvent::push(
                after_row_events,
                event,
            ));
        }
        if let Some(event) = crate::sql::triggers::AfterRowTriggerEvent::prepare(
            engine,
            crate::sql::triggers::AfterRowTriggerInput {
                table: destination_table,
                event: uqa_sql::ast::TriggerEvent::Insert,
                old_doc_id: rewritten_doc_id,
                new_doc_id: rewritten_doc_id,
                old_document: None,
                new_document: Some(&prepared.new_document),
                updated_columns: &[],
                cascade_parent: movement_parent,
            },
        )? {
            last_movement_event = Some(crate::sql::triggers::AfterRowTriggerEvent::push(
                after_row_events,
                event,
            ));
        }
        if prepared.capture_partition_move_update_transition {
            if let Some(updated_columns) = trigger_updated_columns.as_deref() {
                if let Some(event) =
                    crate::sql::triggers::AfterRowTriggerEvent::prepare_transition_capture(
                        engine,
                        crate::sql::triggers::AfterRowTriggerInput {
                            table: &prepared.table,
                            event: uqa_sql::ast::TriggerEvent::Update,
                            old_doc_id: prepared.doc_id,
                            new_doc_id: rewritten_doc_id,
                            old_document: Some(&prepared.old_document),
                            new_document: Some(&prepared.new_document),
                            updated_columns,
                            cascade_parent: movement_parent,
                        },
                    )?
                {
                    last_movement_event = Some(crate::sql::triggers::AfterRowTriggerEvent::push(
                        after_row_events,
                        event,
                    ));
                }
            }
        }
        if let Some(event) = last_movement_event {
            cascade_parent = Some(event);
        }
    } else if let Some(updated_columns) = trigger_updated_columns.as_deref() {
        if let Some(event) = crate::sql::triggers::AfterRowTriggerEvent::prepare(
            engine,
            crate::sql::triggers::AfterRowTriggerInput {
                table: &prepared.table,
                event: uqa_sql::ast::TriggerEvent::Update,
                old_doc_id: prepared.doc_id,
                new_doc_id: rewritten_doc_id,
                old_document: Some(&prepared.old_document),
                new_document: Some(&prepared.new_document),
                updated_columns,
                cascade_parent,
            },
        )? {
            cascade_parent = Some(crate::sql::triggers::AfterRowTriggerEvent::push(
                after_row_events,
                event,
            ));
        }
    }
    for action in &mut prepared.actions {
        stage_prepared_document_rewrite_with_parent(
            engine,
            action,
            params,
            None,
            after_row_events,
            cascade_parent,
        )?;
    }
    Ok(rewritten_doc_id)
}
