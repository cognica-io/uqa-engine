//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    dml_storage_error, document_vectors, integer_primary_key_doc_id,
    lock_document_key_dependencies, lock_existing_document_foreign_key_dependencies,
    lock_existing_document_rewrite_foreign_key_dependencies, lock_mutation_row,
    partition_insert_target, referential_actions, update_lock_strength, DocId, Document, Engine,
    PartitionUpdateRoute, PhysicalDocumentIdentity, PreparedDocumentRewrite,
    ReferentialActionContext, ReferentialRewritePreparation, SQLError, SQLParam,
};

/// Build the complete tuple-lock dependency tree for one rewrite while the backend transaction is still deferred. The prepared documents retain volatile SET DEFAULT results so the apply phase never re-evaluates them.
pub(in crate::sql) fn prepare_document_rewrite(
    engine: &Engine,
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
    engine.lock_relation(table, crate::row_locks::RelationLockMode::RowExclusive)?;
    let changed_columns = old_document
        .keys()
        .chain(new_document.keys())
        .filter(|column| old_document.get(*column) != new_document.get(*column))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    lock_mutation_row(
        engine,
        table,
        table,
        doc_id,
        update_lock_strength(engine, table, &changed_columns),
    )?;
    crate::sql::generated::refresh_stored_generated_columns(engine, table, &mut new_document)?;
    let _key_locks =
        lock_document_key_dependencies(engine, table, &new_document, Some(&old_document))?;
    lock_existing_document_rewrite_foreign_key_dependencies(
        engine,
        table,
        &old_document,
        &new_document,
    )?;
    referential_actions.rewrite_stack.push(key);
    let actions = referential_actions::prepare_referenced_key_update_actions(
        engine,
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

pub(in crate::sql) fn prepare_referential_document_rewrite(
    engine: &Engine,
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
    let Some(new_document) = crate::sql::triggers::fire_before_row_triggers(
        engine,
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
    let route = if let Some(root) = engine.partition_hierarchy_root(table)? {
        let Some(route) = prepare_partition_update_route(
            engine,
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
        engine,
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

fn retarget_prepared_document_rewrite(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
    destination_table: &str,
) -> Result<(), SQLError> {
    if prepared.table == destination_table {
        return Ok(());
    }
    engine.lock_relation(
        destination_table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    let _key_locks =
        lock_document_key_dependencies(engine, destination_table, &prepared.new_document, None)?;
    lock_existing_document_foreign_key_dependencies(
        engine,
        destination_table,
        &prepared.new_document,
    )?;
    let destination_doc_id =
        integer_primary_key_doc_id(engine, destination_table, &prepared.new_document)?
            .unwrap_or(engine.allocate_next_id(destination_table)?);
    prepared.destination = Some((destination_table.to_string(), destination_doc_id));
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
pub(in crate::sql) fn prepare_partition_update_route(
    engine: &Engine,
    storage_table: &str,
    doc_id: DocId,
    old_document: &Document,
    document: Document,
    routing_table: &str,
    params: &[SQLParam],
    include_descendants: bool,
) -> Result<Option<PartitionUpdateRoute>, SQLError> {
    let hierarchy = engine
        .try_table_hierarchy(routing_table)
        .map_err(|error| SQLError::Internal(format!("read rewrite hierarchy: {error}")))?;
    if hierarchy.partition_spec.is_none() && !hierarchy.is_partition() {
        return Ok(Some(PartitionUpdateRoute::Rewrite {
            document,
            destination: None,
        }));
    }
    let destination = partition_insert_target(
        engine,
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
    if crate::sql::triggers::fire_before_row_triggers(
        engine,
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
    engine.lock_relation(
        &destination,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
        engine,
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
    partition_insert_target(engine, &destination, &triggered_document, params, false)?;
    Ok(Some(PartitionUpdateRoute::Rewrite {
        document: triggered_document,
        destination: Some(destination),
    }))
}

pub(in crate::sql) fn prepare_routed_document_rewrite(
    engine: &Engine,
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
                engine,
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
                retarget_prepared_document_rewrite(engine, &mut prepared, &destination)?;
            }
            Ok(Some(prepared))
        }
        PartitionUpdateRoute::Delete { attempted_document } => {
            let delete = super::super::PreparedDocumentDelete {
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

pub(in crate::sql) fn reject_partition_rewrite(
    engine: &Engine,
    prepared: &PreparedDocumentRewrite,
    routing_table: &str,
    params: &[SQLParam],
    include_descendants: bool,
) -> Result<(), SQLError> {
    let hierarchy = engine
        .try_table_hierarchy(routing_table)
        .map_err(|error| SQLError::Internal(format!("read rewrite hierarchy: {error}")))?;
    if hierarchy.partition_spec.is_none() && !hierarchy.is_partition() {
        return Ok(());
    }
    let destination = partition_insert_target(
        engine,
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

pub(in crate::sql) fn apply_validated_prepared_document_rewrite(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
) -> Result<DocId, SQLError> {
    if let Some(delete) = prepared.partition_move_delete.as_mut() {
        super::super::apply_validated_prepared_document_delete(engine, delete)?;
        return Ok(prepared.doc_id);
    }
    if let Some((destination_table, destination_doc_id)) = prepared.destination.as_ref() {
        engine.delete_document(&prepared.table, prepared.doc_id)?;
        engine.add_prepared_document_with_vector_values(
            destination_table,
            *destination_doc_id,
            prepared.new_document.clone(),
            document_vectors(engine, destination_table, &prepared.new_document)?,
            true,
        )?;
        engine
            .advance_next_id(destination_table, *destination_doc_id)
            .map_err(|err| dml_storage_error("UPDATE partition movement", err))?;
        engine.note_row_rewritten_between_tables(
            &prepared.table,
            prepared.doc_id,
            destination_table,
            *destination_doc_id,
        )?;
        engine.defer_rewritten_foreign_key_checks(
            destination_table,
            *destination_doc_id,
            None,
            &prepared.new_document,
        )?;
        for action in &mut prepared.actions {
            apply_validated_prepared_document_rewrite(engine, action)?;
        }
        return Ok(*destination_doc_id);
    }
    let rewritten_doc_id =
        match integer_primary_key_doc_id(engine, &prepared.table, &prepared.new_document)? {
            // An integer primary key names the row's doc_id slot; keep that invariant when the key itself changes, or value -> doc_id lookups (the unique fast path and FOREIGN KEY validation) read the stale slot and miss the row.
            Some(new_id) if new_id != prepared.doc_id => {
                engine.delete_document(&prepared.table, prepared.doc_id)?;
                engine.add_prepared_document_with_vector_values(
                    &prepared.table,
                    new_id,
                    prepared.new_document.clone(),
                    document_vectors(engine, &prepared.table, &prepared.new_document)?,
                    true,
                )?;
                engine
                    .advance_next_id(&prepared.table, new_id)
                    .map_err(|err| dml_storage_error("UPDATE primary key", err))?;
                engine.note_row_rewritten(&prepared.table, prepared.doc_id, new_id)?;
                engine.defer_rewritten_foreign_key_checks(
                    &prepared.table,
                    new_id,
                    Some(&prepared.old_document),
                    &prepared.new_document,
                )?;
                new_id
            }
            _ => {
                engine.rewrite_prepared_document(
                    &prepared.table,
                    prepared.doc_id,
                    prepared.new_document.clone(),
                )?;
                engine.defer_rewritten_foreign_key_checks(
                    &prepared.table,
                    prepared.doc_id,
                    Some(&prepared.old_document),
                    &prepared.new_document,
                )?;
                prepared.doc_id
            }
        };
    for action in &mut prepared.actions {
        apply_validated_prepared_document_rewrite(engine, action)?;
    }
    Ok(rewritten_doc_id)
}
