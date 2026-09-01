//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    coerce_to_column_type, dml_storage_error, eval_lowered_expression,
    lock_physical_mutation_target, missing_document_error, update_lock_strength, DocId, Document,
    Engine, ForeignKey, ForeignKeyAction, ForeignKeyComparison, PhysicalDocumentIdentity,
    PhysicalMutationLockTarget, ReferentialActionContext, SQLError, SQLParam, Value,
};

pub(in crate::sql) fn integer_primary_key_doc_id(
    engine: &Engine,
    table: &str,
    doc: &Document,
) -> Result<Option<DocId>, SQLError> {
    let Some(cols) = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("UPDATE primary key", err))?
    else {
        return Ok(None);
    };
    let Some(pk) = cols.iter().find(|c| c.primary_key && c.ty.is_integer()) else {
        return Ok(None);
    };
    Ok(match doc.get(&pk.name) {
        Some(Value::Int(v)) if *v >= 0 => Some(*v as DocId),
        _ => None,
    })
}

pub(in crate::sql) fn referrers_to_for_actions(
    engine: &Engine,
    table: &str,
) -> Result<Vec<(String, ForeignKey)>, SQLError> {
    if engine.session_replication_role_is_replica() {
        return Ok(Vec::new());
    }
    let mut output = Vec::new();
    for target in engine.hierarchy_ancestor_tables(table)? {
        let referrers = engine
            .try_referrers_to(&target)
            .map_err(|err| dml_storage_error("foreign-key lookup", err))?;
        for (declaring_table, foreign_key) in referrers {
            let referencing_table = engine
                .partition_hierarchy_root(&declaring_table)?
                .unwrap_or(declaring_table);
            if output.iter().any(|(existing_table, existing_key)| {
                existing_table == &referencing_table
                    && foreign_keys_equivalent(existing_key, &foreign_key)
            }) {
                continue;
            }
            output.push((referencing_table, foreign_key));
        }
    }
    Ok(output)
}

fn foreign_keys_equivalent(left: &ForeignKey, right: &ForeignKey) -> bool {
    left.name == right.name
        && left.local_columns == right.local_columns
        && left.ref_table == right.ref_table
        && left.ref_columns == right.ref_columns
        && left.on_update == right.on_update
        && left.on_delete == right.on_delete
        && left.on_delete_set_columns == right.on_delete_set_columns
        && left.match_type == right.match_type
        && left.enforced == right.enforced
}

/// Lock one referencing child row for a referential action and refetch it after the wait. Returns `None` when the child vanished or its foreign-key columns no longer reference the parent key that triggered the action, so the action skips it exactly like `PostgreSQL` after an `EvalPlanQual` recheck of the referencing row.
pub(in crate::sql) struct ReferencingChildLock<'a> {
    pub(in crate::sql) ref_table: &'a str,
    pub(in crate::sql) child: &'a PhysicalDocumentIdentity,
    pub(in crate::sql) lock_columns: &'a [String],
    pub(in crate::sql) foreign_key: &'a ForeignKey,
    pub(in crate::sql) comparison: &'a ForeignKeyComparison,
    pub(in crate::sql) expected: &'a [Value],
}

impl Engine {
    pub(in crate::sql) fn lock_referencing_child(
        &self,
        request: ReferencingChildLock<'_>,
        referential_actions: &ReferentialActionContext,
    ) -> Result<Option<(PhysicalDocumentIdentity, Document)>, SQLError> {
        let ReferencingChildLock {
            ref_table,
            child,
            lock_columns,
            foreign_key,
            comparison,
            expected,
        } = request;
        let target = lock_physical_mutation_target(
            self,
            &child.table,
            ref_table,
            child.doc_id,
            update_lock_strength(self, &child.table, lock_columns),
        )?;
        let PhysicalMutationLockTarget::Present { identity, recheck } = target else {
            return Ok(None);
        };
        if recheck {
            self.refresh_explicit_statement_snapshot()?;
        }
        let child_doc = match referential_actions.pending_document(&identity) {
            Some(Some(document)) => document.clone(),
            Some(None) => return Ok(None),
            None => {
                let Some(document) =
                    self.get_document_for_mutation(&identity.table, identity.doc_id)?
                else {
                    return Ok(None);
                };
                document
            }
        };
        let actual = foreign_key
            .local_columns
            .iter()
            .map(|column| child_doc.get(column).cloned().unwrap_or(Value::Null))
            .collect();
        let actual = comparison.normalize(actual)?;
        Ok((actual == expected).then_some((identity, child_doc)))
    }
}

pub(in crate::sql) fn referencing_rows(
    engine: &Engine,
    table: &str,
    fk: &ForeignKey,
    comparison: &ForeignKeyComparison,
    expected: &[Value],
    referential_actions: &ReferentialActionContext,
) -> Result<Vec<(PhysicalDocumentIdentity, Document)>, SQLError> {
    let mut out = Vec::new();
    for physical_table in engine.hierarchy_scan_tables(table, true)? {
        for doc_id in engine.table_doc_ids(&physical_table)? {
            let identity = PhysicalDocumentIdentity {
                table: physical_table.clone(),
                doc_id,
            };
            let doc = match referential_actions.pending_document(&identity) {
                Some(Some(document)) => document.clone(),
                Some(None) => continue,
                None => {
                    let Some(document) = engine.get_document(&physical_table, doc_id)? else {
                        return Err(missing_document_error(
                            "foreign-key reference scan",
                            &physical_table,
                            doc_id,
                        ));
                    };
                    document
                }
            };
            let values = fk
                .local_columns
                .iter()
                .map(|column| doc.get(column).cloned().unwrap_or(Value::Null))
                .collect();
            if comparison.normalize(values)? == expected {
                out.push((identity, doc));
            }
        }
    }
    Ok(out)
}

pub(in crate::sql) fn apply_set_action_to_child(
    engine: &Engine,
    table: &str,
    old_doc: &Document,
    new_doc: &mut Document,
    columns: &[String],
    action: ForeignKeyAction,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for column in columns {
        let value = match action {
            ForeignKeyAction::SetNull => Value::Null,
            ForeignKeyAction::SetDefault => {
                if let Some(expr) = engine
                    .try_column_default_expr(table, column)
                    .map_err(|err| dml_storage_error("referential SET DEFAULT", err))?
                {
                    eval_lowered_expression(engine, &expr, Some(old_doc), params)?
                } else {
                    Value::Null
                }
            }
            ForeignKeyAction::NoAction | ForeignKeyAction::Restrict | ForeignKeyAction::Cascade => {
                return Err(SQLError::Internal(format!(
                    "invalid SET action helper for `{action:?}`"
                )));
            }
        };
        let value = coerce_to_column_type(engine, table, column, value)?;
        new_doc.insert(column.clone(), value);
    }
    Ok(())
}
