//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stage candidate row images and capture trigger events before durable publication.
use super::{
    constraints::{
        context::ConstraintContext, validate_document_non_key_constraints,
        validate_document_rewrite_constraints, validate_key_constraints, ConstraintStatement,
    },
    prepared::{PreparedDocumentDelete, PreparedDocumentRewrite},
    triggers::context::TriggerContext,
};
use uqa_core::DocId;
use uqa_sql::{SQLError, SQLParam};
use uqa_storage::document_store::Document;
pub trait MutationCommandRows {
    fn stage_shared_command_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Option<std::sync::Arc<Document>>,
    ) -> Result<(), SQLError>;
    fn stage_command_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Option<Document>,
    ) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct MutationStagingContext<'a> {
    pub commands: &'a dyn MutationCommandRows,
    pub constraints: ConstraintContext<'a>,
    pub triggers: TriggerContext<'a>,
}
/// Stage the row that `statement` writes in place of a row, and queue the events of the change.
#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub fn stage_prepared_document_rewrite(
    context: MutationStagingContext<'_>,
    prepared: &mut PreparedDocumentRewrite,
    params: &[SQLParam],
    statement: ConstraintStatement<'_>,
    root_updated_columns: Option<&[String]>,
    after_row_events: &mut Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
) -> Result<DocId, SQLError> {
    let trigger_updated_columns = root_updated_columns
        .or_else(|| prepared.referential_columns())
        .map(<[String]>::to_vec);
    if let Some(delete) = prepared.partition_move_delete.as_deref() {
        stage_document_delete(
            &context,
            delete,
            crate::mutation::referential::checks::moved_row_delete_checks(
                context.constraints,
                &delete.table,
                &delete.document,
            )?,
            after_row_events,
        )?;
        if prepared.capture_partition_move_update_transition {
            if let Some(updated_columns) = trigger_updated_columns.as_deref() {
                if let Some(event) =
                    crate::mutation::triggers::AfterRowTriggerEvent::prepare_transition_capture(
                        &context.triggers,
                        crate::mutation::triggers::AfterRowTriggerInput {
                            table: &prepared.table,
                            event: uqa_sql::ast::TriggerEvent::Update,
                            old_doc_id: prepared.doc_id,
                            new_doc_id: prepared.doc_id,
                            old_document: Some(&prepared.old_document),
                            new_document: None,
                            updated_columns,
                            foreign_key_checks: Vec::new(),
                        },
                    )?
                {
                    after_row_events.push(event);
                }
            }
        }
        return Ok(prepared.doc_id);
    }
    let rewritten_doc_id =
        if let Some((destination_table, destination_doc_id)) = prepared.destination.as_ref() {
            validate_document_non_key_constraints(
                context.constraints,
                Some(statement),
                destination_table,
                &prepared.new_document,
                params,
            )?;
            validate_key_constraints(
                context.constraints,
                destination_table,
                &prepared.new_document,
                None,
            )?;
            context
                .commands
                .stage_command_document(&prepared.table, prepared.doc_id, None)?;
            context.commands.stage_command_document(
                destination_table,
                *destination_doc_id,
                Some(prepared.new_document.clone()),
            )?;
            *destination_doc_id
        } else {
            validate_document_rewrite_constraints(
                context.constraints,
                statement,
                &prepared.table,
                &prepared.old_document,
                &prepared.new_document,
                params,
                prepared.doc_id,
            )?;
            let rewritten_doc_id = prepared.relocation.unwrap_or(prepared.doc_id);
            if rewritten_doc_id != prepared.doc_id {
                context
                    .commands
                    .stage_command_document(&prepared.table, prepared.doc_id, None)?;
            }
            context.commands.stage_command_document(
                &prepared.table,
                rewritten_doc_id,
                Some(prepared.new_document.clone()),
            )?;
            rewritten_doc_id
        };
    if let Some((destination_table, _)) = prepared.destination.as_ref() {
        if let Some(event) = crate::mutation::triggers::AfterRowTriggerEvent::prepare(
            &context.triggers,
            crate::mutation::triggers::AfterRowTriggerInput {
                table: &prepared.table,
                event: uqa_sql::ast::TriggerEvent::Delete,
                old_doc_id: prepared.doc_id,
                new_doc_id: prepared.doc_id,
                old_document: Some(&prepared.old_document),
                new_document: None,
                updated_columns: &[],
                // The table the UPDATE names checks the keys of its foreign keys that the moved row held.
                foreign_key_checks: crate::mutation::referential::checks::moved_row_delete_checks(
                    context.constraints,
                    &prepared.table,
                    &prepared.old_document,
                )?,
            },
        )? {
            after_row_events.push(event);
        }
        if let Some(event) = crate::mutation::triggers::AfterRowTriggerEvent::prepare(
            &context.triggers,
            crate::mutation::triggers::AfterRowTriggerInput {
                table: destination_table,
                event: uqa_sql::ast::TriggerEvent::Insert,
                old_doc_id: rewritten_doc_id,
                new_doc_id: rewritten_doc_id,
                old_document: None,
                new_document: Some(&prepared.new_document),
                updated_columns: &[],
                foreign_key_checks: crate::mutation::referential::checks::referencing_checks(
                    context.constraints,
                    destination_table,
                    rewritten_doc_id,
                    &prepared.new_document,
                    None,
                )?,
            },
        )? {
            after_row_events.push(event);
        }
        if prepared.capture_partition_move_update_transition {
            if let Some(updated_columns) = trigger_updated_columns.as_deref() {
                if let Some(event) =
                    crate::mutation::triggers::AfterRowTriggerEvent::prepare_transition_capture(
                        &context.triggers,
                        crate::mutation::triggers::AfterRowTriggerInput {
                            table: &prepared.table,
                            event: uqa_sql::ast::TriggerEvent::Update,
                            old_doc_id: prepared.doc_id,
                            new_doc_id: rewritten_doc_id,
                            old_document: Some(&prepared.old_document),
                            new_document: Some(&prepared.new_document),
                            updated_columns,
                            foreign_key_checks: Vec::new(),
                        },
                    )?
                {
                    after_row_events.push(event);
                }
            }
        }
        // `PostgreSQL` fires the update triggers of the UPDATE's root for the referenced keys of a row it moved, after the row's insert into its new partition.
        if let Some(event) = crate::mutation::triggers::AfterRowTriggerEvent::foreign_key_checks(
            prepared.moved_through.as_deref().unwrap_or(&prepared.table),
            uqa_sql::ast::TriggerEvent::Update,
            crate::mutation::referential::checks::referenced_checks(
                context.constraints,
                &prepared.table,
                &prepared.old_document,
                Some(&prepared.new_document),
                prepared.moved_through.as_deref(),
            )?,
        ) {
            after_row_events.push(event);
        }
    } else {
        let mut foreign_key_checks = crate::mutation::referential::checks::referenced_checks(
            context.constraints,
            &prepared.table,
            &prepared.old_document,
            Some(&prepared.new_document),
            None,
        )?;
        foreign_key_checks.extend(crate::mutation::referential::checks::referencing_checks(
            context.constraints,
            &prepared.table,
            rewritten_doc_id,
            &prepared.new_document,
            Some(&prepared.old_document),
        )?);
        let event = match trigger_updated_columns.as_deref() {
            Some(updated_columns) => crate::mutation::triggers::AfterRowTriggerEvent::prepare(
                &context.triggers,
                crate::mutation::triggers::AfterRowTriggerInput {
                    table: &prepared.table,
                    event: uqa_sql::ast::TriggerEvent::Update,
                    old_doc_id: prepared.doc_id,
                    new_doc_id: rewritten_doc_id,
                    old_document: Some(&prepared.old_document),
                    new_document: Some(&prepared.new_document),
                    updated_columns,
                    foreign_key_checks,
                },
            )?,
            None => crate::mutation::triggers::AfterRowTriggerEvent::foreign_key_checks(
                &prepared.table,
                uqa_sql::ast::TriggerEvent::Update,
                foreign_key_checks,
            ),
        };
        if let Some(event) = event {
            after_row_events.push(event);
        }
    }
    Ok(rewritten_doc_id)
}

/// Stage a rewrite that a referential action prepared, which writes as a statement of its own that names the foreign key's table and sets its columns.
pub fn stage_referential_rewrite(
    context: MutationStagingContext<'_>,
    action: &mut PreparedDocumentRewrite,
    params: &[SQLParam],
    after_row_events: &mut Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
) -> Result<DocId, SQLError> {
    let referential = action.referential_action.clone().ok_or_else(|| {
        SQLError::Internal("a referential action rewrite does not name its foreign key".into())
    })?;
    stage_prepared_document_rewrite(
        context,
        action,
        params,
        ConstraintStatement::referential_action(&referential.relation, &referential.columns),
        None,
        after_row_events,
    )
}

/// Stage the delete of a row, and queue the events of the change.
pub fn stage_prepared_document_delete(
    context: MutationStagingContext<'_>,
    prepared: &PreparedDocumentDelete,
    after_row_events: &mut Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
) -> Result<(), SQLError> {
    let foreign_key_checks = crate::mutation::referential::checks::referenced_checks(
        context.constraints,
        &prepared.table,
        &prepared.document,
        None,
        None,
    )?;
    stage_document_delete(&context, prepared, foreign_key_checks, after_row_events)
}

fn stage_document_delete(
    context: &MutationStagingContext<'_>,
    prepared: &PreparedDocumentDelete,
    foreign_key_checks: Vec<crate::mutation::referential::checks::ForeignKeyCheck>,
    after_row_events: &mut Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
) -> Result<(), SQLError> {
    context
        .commands
        .stage_command_document(&prepared.table, prepared.doc_id, None)?;
    if let Some(event) = crate::mutation::triggers::AfterRowTriggerEvent::prepare(
        &context.triggers,
        crate::mutation::triggers::AfterRowTriggerInput {
            table: &prepared.table,
            event: uqa_sql::ast::TriggerEvent::Delete,
            old_doc_id: prepared.doc_id,
            new_doc_id: prepared.doc_id,
            old_document: Some(&prepared.document),
            new_document: None,
            updated_columns: &[],
            foreign_key_checks,
        },
    )? {
        after_row_events.push(event);
    }
    Ok(())
}
