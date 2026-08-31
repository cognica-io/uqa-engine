//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger-aware referential-action preparation for referenced-key updates and deletes.

use std::collections::BTreeSet;

use super::super::{prepare_document_delete, PreparedDeleteAction};
use super::{
    apply_set_action_to_child, coerce_to_column_type, foreign_key_comparison_types,
    foreign_key_lookup_values, period_foreign_key_coverage, prepare_referential_document_rewrite,
    referencing_rows, referrers_to_for_actions, DocId, Document, Engine, ForeignKey,
    ForeignKeyAction, PhysicalDocumentIdentity, PreparedDocumentRewrite, ReferencingChildLock,
    ReferentialActionContext, ReferentialRewritePreparation, SQLError, SQLParam, Value,
};

pub(super) fn prepare_referenced_key_update_actions(
    engine: &Engine,
    table: &str,
    parent_doc_id: DocId,
    old_doc: &Document,
    new_doc: &Document,
    params: &[SQLParam],
    referential_actions: &mut ReferentialActionContext,
) -> Result<Vec<PreparedDocumentRewrite>, SQLError> {
    let mut actions = Vec::new();
    for (ref_table, fk) in referrers_to_for_actions(engine, table)? {
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
        engine.lock_relation(&ref_table, crate::row_locks::RelationLockMode::RowExclusive)?;
        let comparison = foreign_key_comparison_types(engine, &ref_table, &fk)?;
        let expected = comparison.normalize(old_values.clone())?;
        let defer_no_action = matches!(fk.on_update, ForeignKeyAction::NoAction)
            && engine.foreign_key_is_deferred(&ref_table, &fk)?;
        if defer_no_action {
            engine.defer_foreign_key_parent_event(&ref_table, table, &fk)?;
        }
        if fk.period {
            let ordinary_len = expected.len().saturating_sub(1);
            let parent = PhysicalDocumentIdentity {
                table: table.to_string(),
                doc_id: parent_doc_id,
            };
            for physical_table in engine.hierarchy_scan_tables(&ref_table, true)? {
                for child_id in engine.table_doc_ids(&physical_table)? {
                    let Some(child_doc) = engine.get_document(&physical_table, child_id)? else {
                        continue;
                    };
                    let Some(child_lookup) =
                        foreign_key_lookup_values(engine, &physical_table, &fk, &child_doc)?
                    else {
                        continue;
                    };
                    if child_lookup.values[..ordinary_len] != expected[..ordinary_len] {
                        continue;
                    }
                    let (covered, _) = period_foreign_key_coverage(
                        engine,
                        &fk,
                        &child_lookup.values,
                        std::slice::from_ref(&parent),
                        Some((&parent, new_doc)),
                    )?;
                    if covered {
                        continue;
                    }
                    if defer_no_action {
                        engine.defer_foreign_key_check(
                            &ref_table,
                            table,
                            &physical_table,
                            child_id,
                            &fk,
                        )?;
                        continue;
                    }
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "update on table \"{table}\" violates foreign key constraint \"{}\" on table \"{ref_table}\"",
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
                engine,
                identity,
                &ref_table,
                uqa_sql::ast::TriggerEvent::Update,
                &fk.local_columns,
            )?;
        }
        let referencing = referencing_rows(
            engine,
            &ref_table,
            &fk,
            &comparison,
            &expected,
            referential_actions,
        )?;
        for (child, _child_doc) in referencing {
            match fk.on_update {
                ForeignKeyAction::NoAction if defer_no_action => {
                    engine.defer_foreign_key_check(
                        &ref_table,
                        table,
                        &child.table,
                        child.doc_id,
                        &fk,
                    )?;
                }
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "update or delete on table \"{table}\" violates foreign key constraint \"{}\" on table \"{ref_table}\"",
                            fk.name.as_deref().unwrap_or("<unnamed>")
                        ),
                    });
                }
                ForeignKeyAction::Cascade => {
                    let Some((child, child_doc)) = engine.lock_referencing_child(
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
                            coerce_to_column_type(engine, &child.table, col, value.clone())?,
                        );
                    }
                    if let Some(prepared) = prepare_referential_document_rewrite(
                        engine,
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
                    let Some((child, child_doc)) = engine.lock_referencing_child(
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
                        engine,
                        &child.table,
                        &child_doc,
                        &mut updated,
                        &fk.local_columns,
                        fk.on_update,
                        params,
                    )?;
                    if let Some(prepared) = prepare_referential_document_rewrite(
                        engine,
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

pub(in crate::sql) fn prepare_referenced_key_delete_actions(
    engine: &Engine,
    parent_table: &str,
    parent_doc_id: DocId,
    parent_document: &Document,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    referential_actions: &mut ReferentialActionContext,
) -> Result<Vec<PreparedDeleteAction>, SQLError> {
    let mut actions = Vec::new();
    for (ref_table, fk) in referrers_to_for_actions(engine, parent_table)? {
        let key_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|column| parent_document.get(column).cloned().unwrap_or(Value::Null))
            .collect();
        if key_values.iter().any(|value| matches!(value, Value::Null)) {
            continue;
        }
        engine.lock_relation(&ref_table, crate::row_locks::RelationLockMode::RowExclusive)?;
        let comparison = foreign_key_comparison_types(engine, &ref_table, &fk)?;
        let expected = comparison.normalize(key_values)?;
        let defer_no_action = matches!(fk.on_delete, ForeignKeyAction::NoAction)
            && engine.foreign_key_is_deferred(&ref_table, &fk)?;
        if defer_no_action {
            engine.defer_foreign_key_parent_event(&ref_table, parent_table, &fk)?;
        }
        if fk.period {
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
            for physical_table in engine.hierarchy_scan_tables(&ref_table, true)? {
                for child_id in engine.table_doc_ids(&physical_table)? {
                    if root_deletes.contains(&(physical_table.clone(), child_id)) {
                        continue;
                    }
                    let Some(child_document) = engine.get_document(&physical_table, child_id)?
                    else {
                        continue;
                    };
                    let Some(child_lookup) =
                        foreign_key_lookup_values(engine, &physical_table, &fk, &child_document)?
                    else {
                        continue;
                    };
                    if child_lookup.values[..ordinary_len] != expected[..ordinary_len] {
                        continue;
                    }
                    if period_foreign_key_coverage(
                        engine,
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
                        engine.defer_foreign_key_check(
                            &ref_table,
                            parent_table,
                            &physical_table,
                            child_id,
                            &fk,
                        )?;
                        continue;
                    }
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "delete on table \"{parent_table}\" violates foreign key constraint \"{}\" on table \"{ref_table}\"",
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
                engine,
                format!("{statement_identity}:on_delete_delete"),
                &ref_table,
                uqa_sql::ast::TriggerEvent::Delete,
                &[],
            )?,
            ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                let columns = delete_set_columns(&fk);
                referential_actions.trigger_statements.begin(
                    engine,
                    format!("{statement_identity}:on_delete_update"),
                    &ref_table,
                    uqa_sql::ast::TriggerEvent::Update,
                    &columns,
                )?;
            }
            ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {}
        }
        let referencing = referencing_rows(
            engine,
            &ref_table,
            &fk,
            &comparison,
            &expected,
            referential_actions,
        )?;
        for (child, _child_document) in referencing {
            if root_deletes.contains(&(child.table.clone(), child.doc_id)) {
                continue;
            }
            match fk.on_delete {
                ForeignKeyAction::NoAction if defer_no_action => {
                    engine.defer_foreign_key_check(
                        &ref_table,
                        parent_table,
                        &child.table,
                        child.doc_id,
                        &fk,
                    )?;
                }
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "update or delete on table \"{parent_table}\" violates foreign key constraint \"{}\" on table \"{ref_table}\"",
                            fk.name.as_deref().unwrap_or("<unnamed>")
                        ),
                    });
                }
                ForeignKeyAction::Cascade => {
                    if let Some(prepared) = prepare_document_delete(
                        engine,
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
                    let Some((child, child_document)) = engine.lock_referencing_child(
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
                        engine,
                        &child.table,
                        &child_document,
                        &mut updated,
                        &columns,
                        fk.on_delete,
                        params,
                    )?;
                    if let Some(prepared) = prepare_referential_document_rewrite(
                        engine,
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
