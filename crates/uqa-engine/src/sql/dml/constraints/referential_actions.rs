//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger-aware ON UPDATE referential-action preparation.

use super::{
    apply_set_action_to_child, coerce_to_column_type, foreign_key_comparison_types,
    foreign_key_lookup_values, lock_referencing_child, period_foreign_key_coverage,
    prepare_referential_document_rewrite, referencing_rows, referrers_to_for_actions, DocId,
    Document, Engine, ForeignKeyAction, PhysicalDocumentIdentity, PreparedDocumentRewrite,
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
                    if fk.deferrable && fk.initially_deferred {
                        engine.defer_foreign_key_row(&physical_table, child_id)?;
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
        let referencing = referencing_rows(engine, &ref_table, &fk, &comparison, &expected)?;
        for (child, _child_doc) in referencing {
            match fk.on_update {
                ForeignKeyAction::NoAction if fk.deferrable && fk.initially_deferred => {
                    engine.defer_foreign_key_row(&child.table, child.doc_id)?;
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
                    let Some((child, child_doc)) = lock_referencing_child(
                        engine,
                        &ref_table,
                        &child,
                        &fk.local_columns,
                        &fk,
                        &comparison,
                        &expected,
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
                    let Some((child, child_doc)) = lock_referencing_child(
                        engine,
                        &ref_table,
                        &child,
                        &fk.local_columns,
                        &fk,
                        &comparison,
                        &expected,
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
