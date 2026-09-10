//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    integer_primary_key_doc_id, lock_document_key_dependencies,
    lock_existing_document_foreign_key_dependencies,
    lock_existing_document_rewrite_foreign_key_dependencies, lock_mutation_row,
    partition_insert_target, prepare_referenced_key_update_actions,
    refresh_stored_generated_columns, update_lock_strength, DocId, Document, PartitionUpdateRoute,
    PhysicalDocumentIdentity, PreparedDocumentDelete, PreparedDocumentRewrite,
    ReferentialActionContext, ReferentialContext, ReferentialRewritePreparation, SQLError,
    SQLParam,
};

/// Build the complete tuple-lock dependency tree for one rewrite while the backend transaction is still deferred. The prepared documents retain volatile SET DEFAULT results so the apply phase never re-evaluates them.
pub fn prepare_document_rewrite<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    doc_id: DocId,
    old_document: Document,
    mut new_document: Document,
    params: &[SQLParam],
    referential_actions: &mut ReferentialActionContext,
) -> Result<Option<PreparedDocumentRewrite>, SQLError> {
    let key = (table.to_string(), doc_id);
    if referential_actions.rewrite_stack.contains(&key) {
        return Ok(None);
    }
    context
        .locking
        .session
        .lock_relation(table, crate::row_locks::RelationLockMode::RowExclusive)?;
    let changed_columns = old_document
        .keys()
        .chain(new_document.keys())
        .filter(|column| old_document.get(*column) != new_document.get(*column))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    lock_mutation_row(
        context.locking.session,
        table,
        table,
        doc_id,
        update_lock_strength(context.locking.catalog, table, &changed_columns),
    )?;
    refresh_stored_generated_columns(context.assignment, table, &mut new_document)?;
    let _key_locks = lock_document_key_dependencies(
        context.constraints,
        table,
        &new_document,
        Some(&old_document),
    )?;
    lock_existing_document_rewrite_foreign_key_dependencies(
        context.constraints,
        table,
        &old_document,
        &new_document,
    )?;
    referential_actions.rewrite_stack.push(key);
    let actions = prepare_referenced_key_update_actions(
        context,
        table,
        doc_id,
        &old_document,
        &new_document,
        params,
        referential_actions,
    );
    referential_actions.rewrite_stack.pop();
    Ok(Some(PreparedDocumentRewrite {
        table: table.to_string(),
        doc_id,
        destination: None,
        partition_move_delete: None,
        old_document,
        new_document,
        actions: actions?,
        trigger_updated_columns: None,
        capture_partition_move_update_transition: true,
    }))
}

pub fn prepare_referential_document_rewrite<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    preparation: ReferentialRewritePreparation<'_>,
    params: &[SQLParam],
    referential_actions: &mut ReferentialActionContext,
) -> Result<Option<PreparedDocumentRewrite>, SQLError> {
    let ReferentialRewritePreparation {
        table,
        doc_id,
        old_document,
        proposed_document,
        updated_columns,
    } = preparation;
    let Some(new_document) = crate::mutation::triggers::fire_before_row_triggers(
        &context.triggers,
        table,
        uqa_sql::ast::TriggerEvent::Update,
        doc_id,
        Some(&old_document),
        Some(&proposed_document),
        &updated_columns,
    )?
    else {
        return Ok(None);
    };
    let route = if let Some(root) = uqa_sql::semantics::partition::partition_hierarchy_root(
        context.constraints.partitions.catalog,
        table,
    )? {
        let Some(route) = prepare_partition_update_route(
            context,
            table,
            doc_id,
            &old_document,
            new_document,
            &root,
            params,
            true,
        )?
        else {
            return Ok(None);
        };
        route
    } else {
        PartitionUpdateRoute::Rewrite {
            document: new_document,
            destination: None,
        }
    };
    let Some(mut prepared) = prepare_routed_document_rewrite(
        context,
        table,
        doc_id,
        old_document,
        route,
        params,
        referential_actions,
    )?
    else {
        return Ok(None);
    };
    prepared.trigger_updated_columns = Some(updated_columns);
    if !prepared.is_partition_move_delete() {
        referential_actions.record_pending_document(
            PhysicalDocumentIdentity {
                table: prepared.table.clone(),
                doc_id: prepared.doc_id,
            },
            Some(prepared.new_document.clone()),
        );
    }
    Ok(Some(prepared))
}

fn retarget_prepared_document_rewrite<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    prepared: &mut PreparedDocumentRewrite,
    destination_table: &str,
) -> Result<(), SQLError> {
    if prepared.table == destination_table {
        return Ok(());
    }
    context.locking.session.lock_relation(
        destination_table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    let _key_locks = lock_document_key_dependencies(
        context.constraints,
        destination_table,
        &prepared.new_document,
        None,
    )?;
    lock_existing_document_foreign_key_dependencies(
        context.constraints,
        destination_table,
        &prepared.new_document,
    )?;
    let destination_doc_id = integer_primary_key_doc_id(
        context.constraints.catalog,
        destination_table,
        &prepared.new_document,
    )?
    .unwrap_or(context.identifiers.allocate_next_id(destination_table)?);
    prepared.destination = Some((destination_table.to_string(), destination_doc_id));
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
pub fn prepare_partition_update_route<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    storage_table: &str,
    doc_id: DocId,
    old_document: &Document,
    document: Document,
    routing_table: &str,
    params: &[SQLParam],
    include_descendants: bool,
) -> Result<Option<PartitionUpdateRoute>, SQLError> {
    let hierarchy = context
        .constraints
        .partitions
        .catalog
        .try_table_hierarchy(routing_table)
        .map_err(|error| SQLError::Internal(format!("read rewrite hierarchy: {error}")))?;
    if hierarchy.partition_spec.is_none() && !hierarchy.is_partition() {
        return Ok(Some(PartitionUpdateRoute::Rewrite {
            document,
            destination: None,
        }));
    }
    let destination = partition_insert_target(
        &context.constraints.partitions,
        routing_table,
        &document,
        params,
        include_descendants,
    )?;
    if destination == storage_table {
        return Ok(Some(PartitionUpdateRoute::Rewrite {
            document,
            destination: None,
        }));
    }
    if crate::mutation::triggers::fire_before_row_triggers(
        &context.triggers,
        storage_table,
        uqa_sql::ast::TriggerEvent::Delete,
        doc_id,
        Some(old_document),
        None,
        &[],
    )?
    .is_none()
    {
        return Ok(None);
    }
    context.locking.session.lock_relation(
        &destination,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    let Some(triggered_document) = crate::mutation::triggers::fire_before_row_triggers(
        &context.triggers,
        &destination,
        uqa_sql::ast::TriggerEvent::Insert,
        doc_id,
        None,
        Some(&document),
        &[],
    )?
    else {
        return Ok(Some(PartitionUpdateRoute::Delete {
            attempted_document: document,
        }));
    };
    partition_insert_target(
        &context.constraints.partitions,
        &destination,
        &triggered_document,
        params,
        false,
    )?;
    Ok(Some(PartitionUpdateRoute::Rewrite {
        document: triggered_document,
        destination: Some(destination),
    }))
}

pub fn prepare_routed_document_rewrite<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    doc_id: DocId,
    old_document: Document,
    route: PartitionUpdateRoute,
    params: &[SQLParam],
    referential_actions: &mut ReferentialActionContext,
) -> Result<Option<PreparedDocumentRewrite>, SQLError> {
    match route {
        PartitionUpdateRoute::Rewrite {
            document,
            destination,
        } => {
            let Some(mut prepared) = prepare_document_rewrite(
                context,
                table,
                doc_id,
                old_document,
                document,
                params,
                referential_actions,
            )?
            else {
                return Ok(None);
            };
            if let Some(destination) = destination {
                retarget_prepared_document_rewrite(context, &mut prepared, &destination)?;
            }
            Ok(Some(prepared))
        }
        PartitionUpdateRoute::Delete { attempted_document } => {
            let delete = PreparedDocumentDelete {
                table: table.to_string(),
                doc_id,
                document: old_document.clone(),
                actions: Vec::new(),
            };
            referential_actions.record_pending_document(
                PhysicalDocumentIdentity {
                    table: table.to_string(),
                    doc_id,
                },
                None,
            );
            Ok(Some(PreparedDocumentRewrite {
                table: table.to_string(),
                doc_id,
                destination: None,
                partition_move_delete: Some(Box::new(delete)),
                old_document,
                new_document: attempted_document,
                actions: Vec::new(),
                trigger_updated_columns: None,
                capture_partition_move_update_transition: true,
            }))
        }
    }
}

pub fn reject_partition_rewrite<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    prepared: &PreparedDocumentRewrite,
    routing_table: &str,
    params: &[SQLParam],
    include_descendants: bool,
) -> Result<(), SQLError> {
    let hierarchy = context
        .constraints
        .partitions
        .catalog
        .try_table_hierarchy(routing_table)
        .map_err(|error| SQLError::Internal(format!("read rewrite hierarchy: {error}")))?;
    if hierarchy.partition_spec.is_none() && !hierarchy.is_partition() {
        return Ok(());
    }
    let destination = partition_insert_target(
        &context.constraints.partitions,
        routing_table,
        &prepared.new_document,
        params,
        include_descendants,
    )?;
    if destination == prepared.table {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "0A000".into(),
        message: "invalid ON UPDATE specification\nDETAIL: The result tuple would appear in a different partition than the original tuple.".into(),
    })
}
