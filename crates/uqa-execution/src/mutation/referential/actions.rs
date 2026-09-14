//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    apply_set_action_to_child, foreign_key_comparison_types, foreign_key_lookup_values,
    lock_referencing_child, period_foreign_key_coverage, prepare_document_delete,
    prepare_referential_document_rewrite, referencing_rows, referrers_to_for_actions, BTreeSet,
    DocId, Document, ForeignKey, ForeignKeyAction, PhysicalDocumentIdentity, PreparedDeleteAction,
    PreparedDocumentRewrite, ReferencingChildLock, ReferentialActionContext, ReferentialContext,
    ReferentialRewritePreparation, SQLError, SQLParam, Value,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves cascade lock and recheck order"
)]
pub fn prepare_referenced_key_update_actions<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    parent_doc_id: DocId,
    old_doc: &Document,
    new_doc: &Document,
    params: &[SQLParam],
    referential_actions: &mut ReferentialActionContext,
) -> Result<Vec<PreparedDocumentRewrite>, SQLError> {
    let mut actions = Vec::new();
    for (ref_table, fk) in referrers_to_for_actions(context.constraints.referrers, table)? {
        let old_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| old_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        let new_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| new_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        if old_values == new_values || old_values.iter().any(|v| matches!(v, Value::Null)) {
            continue;
        }
        context
            .locking
            .session
            .lock_relation(&ref_table, crate::row_locks::RelationLockMode::RowExclusive)?;
        let comparison =
            foreign_key_comparison_types(context.constraints.partitions.catalog, &ref_table, &fk)?;
        let expected = comparison.normalize(old_values.clone())?;
        let defer_no_action = matches!(fk.on_update, ForeignKeyAction::NoAction)
            && context
                .constraints
                .transactions
                .foreign_key_is_deferred(&ref_table, &fk)?;
        if defer_no_action {
            context
                .deferrals
                .defer_foreign_key_parent_event(&ref_table, table, &fk)?;
        }
        if fk.period {
            let snapshot = super::snapshots::ReferenceSnapshot::new(context)?;
            let ordinary_len = expected.len().saturating_sub(1);
            let parent = PhysicalDocumentIdentity {
                table: table.to_string(),
                doc_id: parent_doc_id,
            };
            for physical_table in context
                .constraints
                .catalog
                .hierarchy_scan_tables(&ref_table, true)?
            {
                let rows = snapshot.table(&physical_table)?;
                for child_id in rows.doc_ids()? {
                    let Some(child_doc) = rows.document(child_id)? else {
                        continue;
                    };
                    let Some(child_lookup) = foreign_key_lookup_values(
                        context.constraints.partitions.catalog,
                        &physical_table,
                        &fk,
                        &child_doc,
                    )?
                    else {
                        continue;
                    };
                    if child_lookup.values[..ordinary_len] != expected[..ordinary_len] {
                        continue;
                    }
                    let (covered, _) = period_foreign_key_coverage(
                        context.constraints,
                        &fk,
                        &child_lookup.values,
                        std::slice::from_ref(&parent),
                        Some((&parent, new_doc)),
                    )?;
                    if covered {
                        continue;
                    }
                    if defer_no_action {
                        context.deferrals.defer_foreign_key_check(
                            &ref_table,
                            table,
                            &physical_table,
                            child_id,
                            &fk,
                        )?;
                        continue;
                    }
                    let ref_display = super::foreign_key_relation_name(&ref_table);
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "update on table \"{}\" violates foreign key constraint \"{}\" on table \"{ref_display}\"",
                            super::foreign_key_relation_name(table),
                            fk.name.as_deref().unwrap_or("<unnamed>")
                        ),
                    });
                }
            }
            continue;
        }
        if matches!(
            fk.on_update,
            ForeignKeyAction::Cascade | ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault
        ) {
            let identity = format!(
                "{}:{}:{}:on_update_update",
                ref_table,
                fk.name.as_deref().unwrap_or("<unnamed>"),
                fk.local_columns.join(",")
            );
            referential_actions.trigger_statements.begin(
                &context.triggers,
                identity,
                &ref_table,
                uqa_sql::ast::TriggerEvent::Update,
                &fk.local_columns,
            )?;
        }
        let referencing = referencing_rows(
            context,
            &ref_table,
            &fk,
            &comparison,
            &expected,
            referential_actions,
            fk.on_update,
        )?;
        for (child, _child_doc) in referencing {
            match fk.on_update {
                ForeignKeyAction::NoAction if defer_no_action => {
                    context.deferrals.defer_foreign_key_check(
                        &ref_table,
                        table,
                        &child.table,
                        child.doc_id,
                        &fk,
                    )?;
                }
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    let ref_display = super::foreign_key_relation_name(&ref_table);
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "update or delete on table \"{}\" violates foreign key constraint \"{}\" on table \"{ref_display}\"",
                            super::foreign_key_relation_name(table),
                            fk.name.as_deref().unwrap_or("<unnamed>")
                        ),
                    });
                }
                ForeignKeyAction::Cascade => {
                    let Some((child, child_doc)) = lock_referencing_child(
                        context,
                        ReferencingChildLock {
                            ref_table: &ref_table,
                            child: &child,
                            lock_columns: &fk.local_columns,
                            foreign_key: &fk,
                            comparison: &comparison,
                            expected: &expected,
                        },
                        referential_actions,
                    )?
                    else {
                        continue;
                    };
                    let mut updated = child_doc.clone();
                    for (col, value) in fk.local_columns.iter().zip(new_values.iter()) {
                        updated.insert(
                            col.clone(),
                            uqa_sql::assignment::columns::coerce_to_column_type(
                                context.assignment.assignment,
                                context.assignment.columns,
                                &child.table,
                                col,
                                value.clone(),
                            )?,
                        );
                    }
                    if let Some(prepared) = prepare_referential_document_rewrite(
                        context,
                        ReferentialRewritePreparation {
                            table: &child.table,
                            doc_id: child.doc_id,
                            old_document: child_doc,
                            proposed_document: updated,
                            updated_columns: fk.local_columns.clone(),
                        },
                        params,
                        referential_actions,
                    )? {
                        actions.push(prepared);
                    }
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                    let Some((child, child_doc)) = lock_referencing_child(
                        context,
                        ReferencingChildLock {
                            ref_table: &ref_table,
                            child: &child,
                            lock_columns: &fk.local_columns,
                            foreign_key: &fk,
                            comparison: &comparison,
                            expected: &expected,
                        },
                        referential_actions,
                    )?
                    else {
                        continue;
                    };
                    let mut updated = child_doc.clone();
                    apply_set_action_to_child(
                        context,
                        &child.table,
                        &child_doc,
                        &mut updated,
                        &fk.local_columns,
                        fk.on_update,
                        params,
                    )?;
                    if let Some(prepared) = prepare_referential_document_rewrite(
                        context,
                        ReferentialRewritePreparation {
                            table: &child.table,
                            doc_id: child.doc_id,
                            old_document: child_doc,
                            proposed_document: updated,
                            updated_columns: fk.local_columns.clone(),
                        },
                        params,
                        referential_actions,
                    )? {
                        actions.push(prepared);
                    }
                }
            }
        }
    }
    Ok(actions)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves cascade lock and recheck order"
)]
pub fn prepare_referenced_key_delete_actions<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    parent_table: &str,
    parent_doc_id: DocId,
    parent_document: &Document,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    referential_actions: &mut ReferentialActionContext,
) -> Result<Vec<PreparedDeleteAction>, SQLError> {
    let mut actions = Vec::new();
    for (ref_table, fk) in referrers_to_for_actions(context.constraints.referrers, parent_table)? {
        let key_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|column| parent_document.get(column).cloned().unwrap_or(Value::Null))
            .collect();
        if key_values.iter().any(|value| matches!(value, Value::Null)) {
            continue;
        }
        context
            .locking
            .session
            .lock_relation(&ref_table, crate::row_locks::RelationLockMode::RowExclusive)?;
        let comparison =
            foreign_key_comparison_types(context.constraints.partitions.catalog, &ref_table, &fk)?;
        let expected = comparison.normalize(key_values)?;
        let defer_no_action = matches!(fk.on_delete, ForeignKeyAction::NoAction)
            && context
                .constraints
                .transactions
                .foreign_key_is_deferred(&ref_table, &fk)?;
        if defer_no_action {
            context
                .deferrals
                .defer_foreign_key_parent_event(&ref_table, parent_table, &fk)?;
        }
        if fk.period {
            let snapshot = super::snapshots::ReferenceSnapshot::new(context)?;
            let ordinary_len = expected.len().saturating_sub(1);
            let mut excluded_parents = root_deletes
                .iter()
                .map(|(table, doc_id)| PhysicalDocumentIdentity {
                    table: table.clone(),
                    doc_id: *doc_id,
                })
                .collect::<Vec<_>>();
            let parent_identity = PhysicalDocumentIdentity {
                table: parent_table.to_string(),
                doc_id: parent_doc_id,
            };
            if !excluded_parents.contains(&parent_identity) {
                excluded_parents.push(parent_identity);
            }
            for physical_table in context
                .constraints
                .catalog
                .hierarchy_scan_tables(&ref_table, true)?
            {
                let rows = snapshot.table(&physical_table)?;
                for child_id in rows.doc_ids()? {
                    if root_deletes.contains(&(physical_table.clone(), child_id)) {
                        continue;
                    }
                    let Some(child_document) = rows.document(child_id)? else {
                        continue;
                    };
                    let Some(child_lookup) = foreign_key_lookup_values(
                        context.constraints.partitions.catalog,
                        &physical_table,
                        &fk,
                        &child_document,
                    )?
                    else {
                        continue;
                    };
                    if child_lookup.values[..ordinary_len] != expected[..ordinary_len] {
                        continue;
                    }
                    if period_foreign_key_coverage(
                        context.constraints,
                        &fk,
                        &child_lookup.values,
                        &excluded_parents,
                        None,
                    )?
                    .0
                    {
                        continue;
                    }
                    if defer_no_action {
                        context.deferrals.defer_foreign_key_check(
                            &ref_table,
                            parent_table,
                            &physical_table,
                            child_id,
                            &fk,
                        )?;
                        continue;
                    }
                    let ref_display = super::foreign_key_relation_name(&ref_table);
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "delete on table \"{}\" violates foreign key constraint \"{}\" on table \"{ref_display}\"",
                            super::foreign_key_relation_name(parent_table),
                            fk.name.as_deref().unwrap_or("<unnamed>")
                        ),
                    });
                }
            }
            continue;
        }
        let statement_identity = format!(
            "{}:{}:{}",
            ref_table,
            fk.name.as_deref().unwrap_or("<unnamed>"),
            fk.local_columns.join(",")
        );
        match fk.on_delete {
            ForeignKeyAction::Cascade => referential_actions.trigger_statements.begin(
                &context.triggers,
                format!("{statement_identity}:on_delete_delete"),
                &ref_table,
                uqa_sql::ast::TriggerEvent::Delete,
                &[],
            )?,
            ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                let columns = delete_set_columns(&fk);
                referential_actions.trigger_statements.begin(
                    &context.triggers,
                    format!("{statement_identity}:on_delete_update"),
                    &ref_table,
                    uqa_sql::ast::TriggerEvent::Update,
                    &columns,
                )?;
            }
            ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {}
        }
        let referencing = referencing_rows(
            context,
            &ref_table,
            &fk,
            &comparison,
            &expected,
            referential_actions,
            fk.on_delete,
        )?;
        for (child, _child_document) in referencing {
            if root_deletes.contains(&(child.table.clone(), child.doc_id)) {
                continue;
            }
            match fk.on_delete {
                ForeignKeyAction::NoAction if defer_no_action => {
                    context.deferrals.defer_foreign_key_check(
                        &ref_table,
                        parent_table,
                        &child.table,
                        child.doc_id,
                        &fk,
                    )?;
                }
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    let ref_display = super::foreign_key_relation_name(&ref_table);
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "update or delete on table \"{}\" violates foreign key constraint \"{}\" on table \"{ref_display}\"",
                            super::foreign_key_relation_name(parent_table),
                            fk.name.as_deref().unwrap_or("<unnamed>")
                        ),
                    });
                }
                ForeignKeyAction::Cascade => {
                    if let Some(prepared) = prepare_document_delete(
                        context,
                        &child.table,
                        child.doc_id,
                        params,
                        root_deletes,
                        referential_actions,
                        true,
                    )? {
                        actions.push(PreparedDeleteAction::Delete(Box::new(prepared)));
                    }
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                    let columns = delete_set_columns(&fk);
                    let Some((child, child_document)) = lock_referencing_child(
                        context,
                        ReferencingChildLock {
                            ref_table: &ref_table,
                            child: &child,
                            lock_columns: &columns,
                            foreign_key: &fk,
                            comparison: &comparison,
                            expected: &expected,
                        },
                        referential_actions,
                    )?
                    else {
                        continue;
                    };
                    let mut updated = child_document.clone();
                    apply_set_action_to_child(
                        context,
                        &child.table,
                        &child_document,
                        &mut updated,
                        &columns,
                        fk.on_delete,
                        params,
                    )?;
                    if let Some(prepared) = prepare_referential_document_rewrite(
                        context,
                        ReferentialRewritePreparation {
                            table: &child.table,
                            doc_id: child.doc_id,
                            old_document: child_document,
                            proposed_document: updated,
                            updated_columns: columns,
                        },
                        params,
                        referential_actions,
                    )? {
                        actions.push(PreparedDeleteAction::Rewrite(Box::new(prepared)));
                    }
                }
            }
        }
    }
    Ok(actions)
}

fn delete_set_columns(foreign_key: &ForeignKey) -> Vec<String> {
    if foreign_key.on_delete_set_columns.is_empty() {
        foreign_key.local_columns.clone()
    } else {
        foreign_key.on_delete_set_columns.clone()
    }
}
