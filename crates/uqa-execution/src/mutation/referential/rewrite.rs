//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    arriving_key_identity, defer_updated_key_checks, key_relocation,
    lock_document_key_dependencies, lock_mutation_row, partition_insert_target,
    refresh_stored_generated_columns, update_lock_strength, validate_partition_constraint,
    ConstraintStatement, DocId, Document, PartitionUpdateRoute, PreparedDocumentDelete,
    PreparedDocumentRewrite, ReferentialContext, ReferentialRewrite, ReferentialRewritePreparation,
    SQLError, SQLParam,
};

/// Lock one row for its rewrite while the backend transaction is still deferred, and leave for the transaction the checks of the deferred `NO ACTION` keys the rewrite removes. `referenced_relation` is the relation whose constraints a change to the row's referenced key fires: the row's table, or the relation an `UPDATE` names when the row moves to another partition.
pub fn prepare_document_rewrite<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    referenced_relation: &str,
    doc_id: DocId,
    old_document: Document,
    mut new_document: Document,
) -> Result<PreparedDocumentRewrite, SQLError> {
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
    defer_updated_key_checks(
        context,
        table,
        referenced_relation,
        doc_id,
        &old_document,
        &new_document,
    )?;
    let relocation = key_relocation(
        context.constraints.catalog,
        context.identifiers,
        table,
        doc_id,
        &new_document,
    )?;
    Ok(PreparedDocumentRewrite {
        table: table.to_string(),
        doc_id,
        destination: None,
        moved_through: None,
        relocation,
        partition_move_delete: None,
        old_document,
        new_document,
        capture_partition_move_update_transition: true,
        referential_action: None,
    })
}

/// Prepare the rewrite that a referential action makes of a referencing row: its BEFORE ROW triggers fire, and a partitioned table routes the new row.
pub fn prepare_referential_document_rewrite<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    preparation: ReferentialRewritePreparation<'_>,
    params: &[SQLParam],
) -> Result<Option<PreparedDocumentRewrite>, SQLError> {
    let ReferentialRewritePreparation {
        constraint_table,
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
            ConstraintStatement::referential_action(constraint_table, &updated_columns),
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
            moved_through: None,
        }
    };
    let mut prepared =
        prepare_routed_document_rewrite(context, table, doc_id, old_document, route)?;
    prepared.referential_action = Some(Box::new(ReferentialRewrite {
        relation: constraint_table.to_string(),
        columns: updated_columns,
    }));
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
    let destination_doc_id = match arriving_key_identity(
        context.constraints.catalog,
        context.identifiers,
        destination_table,
        &prepared.new_document,
    )? {
        Some(doc_id) => doc_id,
        None => context.identifiers.allocate_next_id(destination_table)?,
    };
    prepared.destination = Some((destination_table.to_string(), destination_doc_id));
    // The row leaves its table, so it takes no identity there.
    prepared.relocation = None;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
pub fn prepare_partition_update_route<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    statement: ConstraintStatement<'_>,
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
            moved_through: None,
        }));
    }
    let destination = partition_insert_target(
        context.constraints,
        statement,
        routing_table,
        &document,
        params,
        include_descendants,
    )?;
    if hierarchy.partition_spec.is_none() {
        // An UPDATE that names a partition checks its partition constraint before the row's other constraints, and a row that leaves the partition fails it (`ExecCrossPartitionUpdate`).
        validate_partition_constraint(
            context.constraints,
            statement,
            &destination,
            &document,
            params,
        )?;
    }
    if destination == storage_table {
        return Ok(Some(PartitionUpdateRoute::Rewrite {
            document,
            destination: None,
            moved_through: None,
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
    validate_partition_constraint(
        context.constraints,
        statement,
        &destination,
        &triggered_document,
        params,
    )?;
    Ok(Some(PartitionUpdateRoute::Rewrite {
        document: triggered_document,
        destination: Some(destination),
        moved_through: Some(routing_table.to_string()),
    }))
}

pub fn prepare_routed_document_rewrite<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    doc_id: DocId,
    old_document: Document,
    route: PartitionUpdateRoute,
) -> Result<PreparedDocumentRewrite, SQLError> {
    match route {
        PartitionUpdateRoute::Rewrite {
            document,
            destination,
            moved_through,
        } => {
            let mut prepared = prepare_document_rewrite(
                context,
                table,
                moved_through.as_deref().unwrap_or(table),
                doc_id,
                old_document,
                document,
            )?;
            if let Some(destination) = destination {
                retarget_prepared_document_rewrite(context, &mut prepared, &destination)?;
                prepared.moved_through = moved_through;
            }
            Ok(prepared)
        }
        PartitionUpdateRoute::Delete { attempted_document } => {
            let delete = PreparedDocumentDelete {
                table: table.to_string(),
                doc_id,
                document: old_document.clone(),
            };
            Ok(PreparedDocumentRewrite {
                table: table.to_string(),
                doc_id,
                destination: None,
                moved_through: None,
                relocation: None,
                partition_move_delete: Some(Box::new(delete)),
                old_document,
                new_document: attempted_document,
                capture_partition_move_update_transition: true,
                referential_action: None,
            })
        }
    }
}

/// An `ON CONFLICT DO UPDATE` may not move the row it updates to another partition, which it would when the new row fails the partition constraint of the row's partition (`ExecCrossPartitionUpdate`).
pub fn reject_partition_rewrite<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    prepared: &PreparedDocumentRewrite,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    if uqa_sql::semantics::partition::partition_constraint_accepts_row(
        &context.constraints.partitions,
        &prepared.table,
        &prepared.new_document,
        params,
    )? {
        return Ok(());
    }
    Err(SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: "invalid ON UPDATE specification".into(),
        detail: Some(
            "The result tuple would appear in a different partition than the original tuple."
                .into(),
        ),
        hint: None,
    })
}
