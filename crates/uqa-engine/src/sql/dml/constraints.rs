//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row, key, foreign-key, and referential-action validation.

mod period;
mod referential_actions;

use super::{
    coerce_to_column_type, dml_storage_error, document_vectors, eval_lowered_expression,
    lock_mutation_row, lock_mutation_target, lock_physical_mutation_target, missing_document_error,
    partition_insert_target, update_lock_strength, ColumnType, DocId, Document, Engine, ForeignKey,
    ForeignKeyAction, ForeignKeyMatch, MutationLockTarget, PhysicalDocumentIdentity,
    PhysicalMutationLockTarget, PreparedDocumentRewrite, ReferentialActionContext,
    ReferentialRewritePreparation, SQLError, SQLParam, Value,
};
use sha2::{Digest, Sha256};
use uqa_sql::ast::TableKeyConstraint;

pub(in crate::sql) use period::period_foreign_key_coverage;
use period::period_ranges;
use referential_actions::prepare_referenced_key_update_actions;

pub(in crate::sql) struct ForeignKeyLookup {
    pub(in crate::sql) values: Vec<Value>,
    comparison: ForeignKeyComparison,
}

pub(in crate::sql) struct ForeignKeyComparison {
    comparison_types: Vec<ColumnType>,
    exact_reference_lookup: bool,
}

impl ForeignKeyComparison {
    pub(in crate::sql) fn normalize(&self, values: Vec<Value>) -> Result<Vec<Value>, SQLError> {
        normalize_foreign_key_values(values, &self.comparison_types)
    }
}

pub(in crate::sql) fn validate_document_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
    ignored_doc_id: Option<DocId>,
) -> Result<(), SQLError> {
    validate_document_non_key_constraints(engine, table, document, params)?;
    validate_key_constraints(engine, table, document, ignored_doc_id)
}

pub(in crate::sql) fn validate_document_rewrite_constraints(
    engine: &Engine,
    table: &str,
    old_document: &Document,
    new_document: &Document,
    params: &[SQLParam],
    doc_id: DocId,
) -> Result<(), SQLError> {
    validate_document_non_key_constraints_with_old(
        engine,
        table,
        new_document,
        params,
        Some(old_document),
    )?;
    validate_key_constraints(engine, table, new_document, Some(doc_id))
}

pub(in crate::sql) fn validate_document_non_key_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    validate_document_non_key_constraints_with_old(engine, table, document, params, None)
}

fn validate_document_non_key_constraints_with_old(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
    old_document: Option<&Document>,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let check_constraints = engine
        .try_check_constraint_definitions(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?;
    let schema = uqa_execution::RowSchema::with_types(
        definitions
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        definitions
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let virtual_columns = definitions
        .iter()
        .filter(|column| {
            column.generated.as_ref().is_some_and(|generated| {
                generated.kind == uqa_sql::ast::GeneratedColumnKind::Virtual
            })
        })
        .collect::<Vec<_>>();
    let mut required_virtual_columns = std::collections::BTreeSet::new();
    for column in &virtual_columns {
        if column.not_null
            || check_constraints.iter().any(|constraint| {
                constraint.enforced
                    && crate::engine_table_storage::schema_expr_references_column(
                        &constraint.expr,
                        &column.name,
                    )
            })
        {
            required_virtual_columns.insert(column.name.clone());
        }
    }
    let logical_document = if required_virtual_columns.is_empty() {
        None
    } else {
        let mut logical_document = document.clone();
        crate::engine_generated::materialize_selected_virtual_generated_columns(
            &definitions,
            &mut logical_document,
            &required_virtual_columns,
        )?;
        Some(logical_document)
    };
    let document = logical_document.as_ref().unwrap_or(document);

    for col_def in &definitions {
        if !col_def.not_null
            || col_def.auto_increment.as_ref().is_some_and(|provenance| {
                provenance.kind == uqa_sql::ast::AutoIncrementKind::Legacy
            })
        {
            continue;
        }
        match document.get(&col_def.name) {
            Some(Value::Null) | None => {
                return Err(SQLError::Routine {
                    sqlstate: "23502".into(),
                    message: format!(
                        "null value in column \"{}\" of relation \"{table}\" violates not-null constraint",
                        col_def.name
                    ),
                });
            }
            _ => {}
        }
    }

    for constraint in check_constraints {
        if !constraint.enforced {
            continue;
        }
        let accepted = if let Some(partition) = constraint.partition_constraint.as_ref() {
            crate::sql::partition_constraint_accepts_document(
                engine,
                table,
                &partition.spec,
                &partition.bound,
                document,
            )?
        } else {
            let result = crate::sql::scalar::eval_lowered_expression_with_schema(
                engine,
                &constraint.expr,
                document,
                &schema,
                params,
            )?;
            matches!(result, Value::Null) || uqa_sql::expr::truthy(&result)
        };
        if !accepted {
            let label = constraint.name.unwrap_or_else(|| "<unnamed>".into());
            let relation = crate::RelationIdentity::from_legacy_name(table)
                .map_or_else(|_| table.to_string(), |identity| identity.name);
            return Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: format!(
                    "new row for relation \"{relation}\" violates check constraint \"{label}\""
                ),
            });
        }
    }

    lock_document_foreign_key_dependencies(engine, table, document, false, old_document)
}

/// Acquire every referenced-parent tuple lock that already exists without rejecting a temporarily missing parent. INSERT uses this as a lock-only preflight for all input rows before taking the backend writer; ordinary constraint validation still runs in row order afterwards, so a self-referencing row can see a parent inserted earlier by the same statement and a genuinely missing parent still raises the normal error.
pub(in crate::sql) fn lock_existing_document_foreign_key_dependencies(
    engine: &Engine,
    table: &str,
    document: &Document,
) -> Result<(), SQLError> {
    lock_document_foreign_key_dependencies(engine, table, document, true, None)
}

pub(in crate::sql) fn lock_existing_document_rewrite_foreign_key_dependencies(
    engine: &Engine,
    table: &str,
    old_document: &Document,
    new_document: &Document,
) -> Result<(), SQLError> {
    lock_document_foreign_key_dependencies(engine, table, new_document, true, Some(old_document))
}

fn lock_document_foreign_key_dependencies(
    engine: &Engine,
    table: &str,
    document: &Document,
    allow_missing: bool,
    old_document: Option<&Document>,
) -> Result<(), SQLError> {
    for fk in engine
        .try_foreign_keys(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        if !fk.enforced {
            continue;
        }
        if !allow_missing && engine.foreign_key_is_deferred(table, &fk)? {
            continue;
        }
        if old_document.is_some_and(|old_document| {
            fk.local_columns.iter().all(|column| {
                old_document.get(column).cloned().unwrap_or(Value::Null)
                    == document.get(column).cloned().unwrap_or(Value::Null)
            })
        }) {
            continue;
        }
        let Some(local_values) = foreign_key_lookup_values(engine, table, &fk, document)? else {
            continue;
        };
        let violation = || SQLError::Routine {
            sqlstate: "23503".into(),
            message: format!(
                "insert or update on table \"{table}\" violates foreign key constraint \"{}\"",
                fk.name.as_deref().unwrap_or("<unnamed>")
            ),
        };
        if fk.period {
            let (covered, parent_ids) =
                period_foreign_key_coverage(engine, &fk, &local_values.values, &[], None)?;
            if !covered {
                if allow_missing {
                    continue;
                }
                return Err(violation());
            }
            for parent in parent_ids {
                let _target = lock_mutation_target(
                    engine,
                    &parent.table,
                    &fk.ref_table,
                    parent.doc_id,
                    uqa_sql::ast::LockStrength::ForKeyShare,
                )?;
            }
            continue;
        }
        let mut hops = 0usize;
        loop {
            let Some(parent) = find_foreign_key_parent(engine, &fk, &local_values)? else {
                if allow_missing {
                    break;
                }
                return Err(violation());
            };
            // PostgreSQL 18 holds FOR KEY SHARE on the referenced row until the referencing transaction ends. If the lookup waits, refresh the READ COMMITTED snapshot and follow a delete/reinsert or key rewrite until the tuple carrying the requested key is locked.
            let target = lock_mutation_target(
                engine,
                &parent.table,
                &fk.ref_table,
                parent.doc_id,
                uqa_sql::ast::LockStrength::ForKeyShare,
            )?;
            let MutationLockTarget::Present {
                doc_id: locked_parent,
                recheck,
            } = target
            else {
                engine.refresh_explicit_statement_snapshot()?;
                hops += 1;
                if hops > 64 {
                    return Err(SQLError::Internal(format!(
                        "foreign-key parent lookup for `{table}` did not converge"
                    )));
                }
                continue;
            };
            if recheck {
                engine.refresh_explicit_statement_snapshot()?;
            }
            let locked_parent = PhysicalDocumentIdentity {
                table: parent.table,
                doc_id: locked_parent,
            };
            match find_foreign_key_parent(engine, &fk, &local_values)? {
                Some(current_parent) if current_parent == locked_parent => break,
                None if allow_missing => break,
                None => return Err(violation()),
                Some(_) => {
                    hops += 1;
                    if hops > 64 {
                        return Err(SQLError::Internal(format!(
                            "foreign-key parent lookup for `{table}` did not converge"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

struct DeferredForeignKeyValidation {
    foreign_key: ForeignKey,
    comparison: Option<ForeignKeyComparison>,
    cross_type_parent_keys: Option<std::collections::BTreeSet<Vec<Value>>>,
}

pub(crate) fn validate_deferred_foreign_key_checks(
    engine: &Engine,
    checks: &[crate::DeferredForeignKeyCheck],
    targets: Option<&std::collections::BTreeSet<crate::ConstraintIdentity>>,
) -> Result<(), SQLError> {
    let mut validation_by_constraint = std::collections::BTreeMap::new();
    for check in checks {
        let selected = targets.is_none_or(|targets| {
            targets.iter().any(|target| {
                crate::engine_transactions::constraint_identities_match(target, &check.constraint)
            })
        });
        if !selected {
            continue;
        }
        let Some(row) = check.row else {
            continue;
        };
        let table = engine.row_lock_manager().table_name(row.table).to_string();
        let cache_key = (check.constraint.clone(), table.clone());
        if !validation_by_constraint.contains_key(&cache_key) {
            let constraint_table = check.constraint.relation.qualified_name();
            let foreign_key = engine
                .try_foreign_keys(&constraint_table)
                .map_err(|error| dml_storage_error("deferred constraint validation", error))?
                .into_iter()
                .find(|foreign_key| {
                    foreign_key.name.as_deref() == Some(&check.constraint.name)
                        && foreign_key.object_id == check.constraint.object_id
                });
            let validation = if let Some(foreign_key) =
                foreign_key.filter(|foreign_key| foreign_key.enforced)
            {
                if foreign_key.period {
                    Some(DeferredForeignKeyValidation {
                        foreign_key,
                        comparison: None,
                        cross_type_parent_keys: None,
                    })
                } else {
                    let comparison = foreign_key_comparison_types(engine, &table, &foreign_key)?;
                    let cross_type_parent_keys = if comparison.exact_reference_lookup {
                        None
                    } else {
                        Some(foreign_key_parent_index(engine, &foreign_key, &comparison)?)
                    };
                    Some(DeferredForeignKeyValidation {
                        foreign_key,
                        comparison: Some(comparison),
                        cross_type_parent_keys,
                    })
                }
            } else {
                None
            };
            validation_by_constraint.insert(cache_key.clone(), validation);
        }
        let Some(validation) = validation_by_constraint
            .get(&cache_key)
            .and_then(Option::as_ref)
        else {
            continue;
        };
        let Some(document) = engine.get_document(&table, row.doc_id)? else {
            continue;
        };
        if validation.foreign_key.period {
            let Some(lookup) =
                foreign_key_lookup_values(engine, &table, &validation.foreign_key, &document)?
            else {
                continue;
            };
            if period_foreign_key_coverage(
                engine,
                &validation.foreign_key,
                &lookup.values,
                &[],
                None,
            )?
            .0
            {
                continue;
            }
        } else {
            let comparison = validation.comparison.as_ref().ok_or_else(|| {
                SQLError::Internal("deferred foreign-key comparison was not prepared".into())
            })?;
            let Some(values) = foreign_key_values(&validation.foreign_key, &document, comparison)?
            else {
                continue;
            };
            let parent_exists = if comparison.exact_reference_lookup {
                find_exact_foreign_key_parent(engine, &validation.foreign_key, &values)?.is_some()
            } else {
                validation
                    .cross_type_parent_keys
                    .as_ref()
                    .is_some_and(|keys| keys.contains(&values))
            };
            if parent_exists {
                continue;
            }
        }
        return Err(SQLError::Routine {
            sqlstate: "23503".into(),
            message: format!(
                "insert or update on table \"{table}\" violates foreign key constraint \"{}\"",
                validation
                    .foreign_key
                    .name
                    .as_deref()
                    .unwrap_or("<unnamed>")
            ),
        });
    }
    Ok(())
}

pub(in crate::sql) fn key_constraint_values(
    constraint: &uqa_sql::ast::TableKeyConstraint,
    document: &Document,
) -> Option<Vec<Value>> {
    let values: Vec<Value> = constraint
        .columns
        .iter()
        .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
        .collect();
    if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
        && !constraint.nulls_not_distinct
        && values.iter().any(|value| matches!(value, Value::Null))
    {
        return None;
    }
    Some(values)
}

fn period_values_overlap(
    left: &Value,
    right: &Value,
    column_type: &ColumnType,
) -> Result<bool, SQLError> {
    let (left_subtype, left) = period_ranges(left, column_type)?;
    let (right_subtype, right) = period_ranges(right, column_type)?;
    Ok(left_subtype == right_subtype
        && left
            .iter()
            .any(|left| right.iter().any(|right| left.overlaps(right))))
}

pub(in crate::sql) fn without_overlaps_conflict(
    engine: &Engine,
    table: &str,
    constraint: &TableKeyConstraint,
    document: &Document,
    ignored_doc_id: Option<DocId>,
) -> Result<bool, SQLError> {
    let Some(period_column) = constraint.columns.last() else {
        return Err(SQLError::Internal(
            "WITHOUT OVERLAPS constraint has no period column".into(),
        ));
    };
    let period_type = engine
        .column_type(table, period_column)
        .map_err(|error| dml_storage_error("WITHOUT OVERLAPS type lookup", error))?
        .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{period_column}")))?;
    let candidate_period = document.get(period_column).cloned().unwrap_or(Value::Null);
    if matches!(candidate_period, Value::Null) {
        return Ok(false);
    }
    let (_, candidate_ranges) = period_ranges(&candidate_period, &period_type)?;
    if candidate_ranges.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "23514".into(),
            message: format!(
                "empty WITHOUT OVERLAPS value found in column \"{period_column}\" in relation \"{table}\""
            ),
        });
    }
    let ordinary_columns = &constraint.columns[..constraint.columns.len() - 1];
    for doc_id in engine.table_doc_ids(table)? {
        if ignored_doc_id == Some(doc_id) {
            continue;
        }
        let Some(existing) = engine.get_document(table, doc_id)? else {
            return Err(missing_document_error(
                "WITHOUT OVERLAPS scan",
                table,
                doc_id,
            ));
        };
        if !ordinary_columns.iter().all(|column| {
            existing.get(column).cloned().unwrap_or(Value::Null)
                == document.get(column).cloned().unwrap_or(Value::Null)
        }) {
            continue;
        }
        let existing_period = existing.get(period_column).cloned().unwrap_or(Value::Null);
        if matches!(existing_period, Value::Null) {
            continue;
        }
        if period_values_overlap(&candidate_period, &existing_period, &period_type)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reserve every UNIQUE / PRIMARY KEY value that a new row can publish, or every such value changed by a rewrite, before the backend writer is held. The reservation is the logical equivalent of `PostgreSQL`'s speculative index-tuple wait: a deferred reader that cannot yet see another writer's uncommitted row waits on the exact key, refreshes its snapshot, and only then decides whether INSERT or ON CONFLICT applies.
pub(in crate::sql) fn lock_document_key_dependencies(
    engine: &Engine,
    table: &str,
    document: &Document,
    old_document: Option<&Document>,
) -> Result<Vec<crate::row_locks::RowLockAcquisition>, SQLError> {
    let canonical_table = engine
        .try_resolve_table_name(table)
        .map_err(|error| dml_storage_error("key-lock table resolution", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let constraints = engine
        .try_key_constraints(&canonical_table)
        .map_err(|error| dml_storage_error("key-lock constraint lookup", error))?;
    let mut lock_keys = std::collections::BTreeSet::new();
    for constraint in constraints {
        let Some(values) = key_constraint_values(&constraint, document) else {
            continue;
        };
        if old_document.is_some_and(|old_document| {
            key_constraint_values(&constraint, old_document).as_ref() == Some(&values)
        }) {
            continue;
        }
        let lock_values = if constraint.without_overlaps {
            &values[..values.len().saturating_sub(1)]
        } else {
            values.as_slice()
        };
        let key = uqa_execution::canonical_row_key(lock_values)
            .map_err(crate::sql::select::physical_exec_error)?;
        let mut digest = Sha256::new();
        digest.update(b"uqa-key-lock-v1");
        update_key_lock_digest(&mut digest, canonical_table.as_bytes())?;
        digest.update([match constraint.kind {
            uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => 0,
            uqa_sql::ast::TableKeyConstraintKind::Unique => 1,
        }]);
        digest.update([u8::from(constraint.nulls_not_distinct)]);
        digest.update([u8::from(constraint.without_overlaps)]);
        for column in &constraint.columns {
            update_key_lock_digest(&mut digest, column.as_bytes())?;
        }
        update_key_lock_digest(&mut digest, &key)?;
        let digest: [u8; 32] = digest.finalize().into();
        lock_keys.insert(digest);
    }

    let mut acquisitions = Vec::new();
    let mut waited = false;
    for lock_key in lock_keys {
        match engine.lock_key_reservation(lock_key, table)? {
            crate::row_locks::LockAcquire::Granted {
                acquisition,
                waited: lock_waited,
                ..
            } => {
                waited |= lock_waited;
                acquisitions.extend(acquisition);
            }
            crate::row_locks::LockAcquire::Skipped => {
                return Err(SQLError::Internal(
                    "blocking key reservation unexpectedly skipped a key".into(),
                ));
            }
        }
    }
    if waited {
        engine.refresh_explicit_statement_snapshot()?;
    }
    Ok(acquisitions)
}

fn update_key_lock_digest(digest: &mut Sha256, part: &[u8]) -> Result<(), SQLError> {
    let len = u64::try_from(part.len())
        .map_err(|_| SQLError::Internal("key-lock digest part exceeds u64".into()))?;
    digest.update(len.to_be_bytes());
    digest.update(part);
    Ok(())
}

pub(in crate::sql) fn validate_key_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    ignored_doc_id: Option<DocId>,
) -> Result<(), SQLError> {
    for constraint in engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        let Some(values) = key_constraint_values(&constraint, document) else {
            continue;
        };
        if constraint.without_overlaps {
            if !without_overlaps_conflict(engine, table, &constraint, document, ignored_doc_id)? {
                continue;
            }
            let name = constraint.name.as_deref().unwrap_or("<unnamed>");
            return Err(SQLError::Routine {
                sqlstate: "23P01".into(),
                message: format!("conflicting key value violates exclusion constraint \"{name}\""),
            });
        }
        let Some(conflict_id) = engine.find_conflict(table, &constraint.columns, &values)? else {
            continue;
        };
        if ignored_doc_id == Some(conflict_id) {
            continue;
        }
        let kind = match constraint.kind {
            uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
            uqa_sql::ast::TableKeyConstraintKind::Unique => "UNIQUE",
        };
        let name = constraint
            .name
            .as_deref()
            .map_or_else(String::new, |name| format!(" `{name}`"));
        return Err(SQLError::Routine {
            sqlstate: "23505".into(),
            message: format!(
                "{kind} constraint{name} violated: duplicate value for columns ({}) in table `{table}`",
                constraint.columns.join(", ")
            ),
        });
    }
    Ok(())
}

pub(in crate::sql) fn foreign_key_lookup_values(
    engine: &Engine,
    table: &str,
    fk: &ForeignKey,
    document: &Document,
) -> Result<Option<ForeignKeyLookup>, SQLError> {
    let comparison = foreign_key_comparison_types(engine, table, fk)?;
    Ok(foreign_key_values(fk, document, &comparison)?
        .map(|values| ForeignKeyLookup { values, comparison }))
}

fn foreign_key_values(
    fk: &ForeignKey,
    document: &Document,
    comparison: &ForeignKeyComparison,
) -> Result<Option<Vec<Value>>, SQLError> {
    let local_values: Vec<Value> = fk
        .local_columns
        .iter()
        .map(|c| document.get(c).cloned().unwrap_or(Value::Null))
        .collect();
    let null_count = local_values
        .iter()
        .filter(|value| matches!(value, Value::Null))
        .count();
    if null_count == 0 {
        if local_values.len() != fk.ref_columns.len() {
            return Err(SQLError::Internal(
                "FOREIGN KEY local and referenced column counts diverged after validation".into(),
            ));
        }
        return comparison.normalize(local_values).map(Some);
    }
    match fk.match_type {
        ForeignKeyMatch::Simple => Ok(None),
        ForeignKeyMatch::Full if null_count == local_values.len() => Ok(None),
        ForeignKeyMatch::Full => {
            Err(SQLError::Routine {
                sqlstate: "23503".into(),
                message: format!(
                    "insert or update on table violates foreign key constraint \"{}\": MATCH FULL does not allow mixing of null and nonnull key values",
                    fk.name.as_deref().unwrap_or("<unnamed>")
                ),
            })
        }
    }
}

pub(in crate::sql) fn find_foreign_key_parent(
    engine: &Engine,
    fk: &ForeignKey,
    lookup: &ForeignKeyLookup,
) -> Result<Option<PhysicalDocumentIdentity>, SQLError> {
    if lookup.comparison.exact_reference_lookup {
        return find_exact_foreign_key_parent(engine, fk, &lookup.values);
    }
    for physical_table in engine.hierarchy_scan_tables(&fk.ref_table, true)? {
        for doc_id in engine.table_doc_ids(&physical_table)? {
            let Some(document) = engine.get_document(&physical_table, doc_id)? else {
                continue;
            };
            if foreign_key_parent_values(fk, &document, &lookup.comparison)? == lookup.values {
                return Ok(Some(PhysicalDocumentIdentity {
                    table: physical_table.clone(),
                    doc_id,
                }));
            }
        }
    }
    Ok(None)
}

fn find_exact_foreign_key_parent(
    engine: &Engine,
    fk: &ForeignKey,
    values: &[Value],
) -> Result<Option<PhysicalDocumentIdentity>, SQLError> {
    for physical_table in engine.hierarchy_scan_tables(&fk.ref_table, true)? {
        if let Some(doc_id) = engine.find_conflict(&physical_table, &fk.ref_columns, values)? {
            return Ok(Some(PhysicalDocumentIdentity {
                table: physical_table,
                doc_id,
            }));
        }
    }
    Ok(None)
}

pub(in crate::sql) fn foreign_key_comparison_types(
    engine: &Engine,
    table: &str,
    fk: &ForeignKey,
) -> Result<ForeignKeyComparison, SQLError> {
    if fk.local_columns.len() != fk.ref_columns.len() {
        return Err(SQLError::Internal(
            "FOREIGN KEY local and referenced column counts diverged after validation".into(),
        ));
    }
    let local_columns = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("FOREIGN KEY local columns", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let referenced_columns = engine
        .try_describe_table(&fk.ref_table)
        .map_err(|error| dml_storage_error("FOREIGN KEY referenced columns", error))?
        .ok_or_else(|| SQLError::UnknownTable(fk.ref_table.clone()))?;
    let mut comparison_types = Vec::with_capacity(fk.local_columns.len());
    let mut exact_reference_lookup = true;
    for (local_column, referenced_column) in fk.local_columns.iter().zip(&fk.ref_columns) {
        let local_type = local_columns
            .iter()
            .find(|definition| definition.name == *local_column)
            .map(|definition| &definition.ty)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{local_column}")))?;
        let referenced_type = referenced_columns
            .iter()
            .find(|definition| definition.name == *referenced_column)
            .map(|definition| &definition.ty)
            .ok_or_else(|| {
                SQLError::UnknownColumn(format!("{}.{referenced_column}", fk.ref_table))
            })?;
        let comparison_type =
            uqa_execution::foreign_key_operand_type(local_type, referenced_type).map_err(|_| {
                SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: format!(
                        "foreign key constraint cannot be implemented: key columns \"{local_column}\" and \"{referenced_column}\" are of incompatible types: {} and {}",
                        local_type.sql_name(),
                        referenced_type.sql_name()
                    ),
                }
            })?;
        exact_reference_lookup &= comparison_type == *referenced_type;
        comparison_types.push(comparison_type);
    }
    Ok(ForeignKeyComparison {
        comparison_types,
        exact_reference_lookup,
    })
}

fn foreign_key_parent_values(
    fk: &ForeignKey,
    document: &Document,
    comparison: &ForeignKeyComparison,
) -> Result<Vec<Value>, SQLError> {
    comparison.normalize(
        fk.ref_columns
            .iter()
            .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
            .collect(),
    )
}

fn foreign_key_parent_index(
    engine: &Engine,
    fk: &ForeignKey,
    comparison: &ForeignKeyComparison,
) -> Result<std::collections::BTreeSet<Vec<Value>>, SQLError> {
    let mut keys = std::collections::BTreeSet::new();
    for physical_table in engine.hierarchy_scan_tables(&fk.ref_table, true)? {
        for doc_id in engine.table_doc_ids(&physical_table)? {
            let Some(document) = engine.get_document(&physical_table, doc_id)? else {
                continue;
            };
            keys.insert(foreign_key_parent_values(fk, &document, comparison)?);
        }
    }
    Ok(keys)
}

fn normalize_foreign_key_values(
    values: Vec<Value>,
    comparison_types: &[ColumnType],
) -> Result<Vec<Value>, SQLError> {
    if values.len() != comparison_types.len() {
        return Err(SQLError::Internal(
            "FOREIGN KEY value and comparison-type counts diverged after validation".into(),
        ));
    }
    values
        .into_iter()
        .zip(comparison_types)
        .map(|(value, ty)| crate::sql::ddl::convert_value_to_column_type(value, ty))
        .collect()
}

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
    super::stamp_tuple_xmin(engine, table, &mut new_document)?;
    let _key_locks =
        lock_document_key_dependencies(engine, table, &new_document, Some(&old_document))?;
    lock_existing_document_rewrite_foreign_key_dependencies(
        engine,
        table,
        &old_document,
        &new_document,
    )?;
    referential_actions.rewrite_stack.push(key);
    let actions = prepare_referenced_key_update_actions(
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
        old_document,
        new_document,
        actions: actions?,
        trigger_updated_columns: None,
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
    let Some(mut prepared) = prepare_document_rewrite(
        engine,
        table,
        doc_id,
        old_document,
        new_document,
        params,
        referential_actions,
    )?
    else {
        return Ok(None);
    };
    prepared.trigger_updated_columns = Some(updated_columns);
    finalize_referential_partition_rewrite(engine, &mut prepared, params)?;
    Ok(Some(prepared))
}

pub(in crate::sql) fn retarget_prepared_document_rewrite(
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::sql) enum PartitionRewritePolicy {
    Move,
    RejectOnConflict,
}

pub(in crate::sql) fn finalize_partition_rewrite(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
    routing_table: &str,
    params: &[SQLParam],
    include_descendants: bool,
    policy: PartitionRewritePolicy,
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
    if policy == PartitionRewritePolicy::RejectOnConflict {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "invalid ON UPDATE specification\nDETAIL: The result tuple would appear in a different partition than the original tuple.".into(),
        });
    }
    retarget_prepared_document_rewrite(engine, prepared, &destination)
}

pub(in crate::sql) fn finalize_referential_partition_rewrite(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let Some(root) = engine.partition_hierarchy_root(&prepared.table)? else {
        return Ok(());
    };
    finalize_partition_rewrite(
        engine,
        prepared,
        &root,
        params,
        true,
        PartitionRewritePolicy::Move,
    )
}

pub(in crate::sql) fn stage_prepared_document_rewrite(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
    params: &[SQLParam],
    root_updated_columns: Option<&[String]>,
    after_row_events: &mut Vec<crate::sql::triggers::AfterRowTriggerEvent>,
) -> Result<DocId, SQLError> {
    let trigger_updated_columns = root_updated_columns
        .map(<[String]>::to_vec)
        .or_else(|| prepared.trigger_updated_columns.clone());
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
    if let Some(updated_columns) = trigger_updated_columns.as_deref() {
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
            },
        )? {
            after_row_events.push(event);
        }
    }
    for action in &mut prepared.actions {
        stage_prepared_document_rewrite(engine, action, params, None, after_row_events)?;
    }
    Ok(rewritten_doc_id)
}

pub(in crate::sql) fn apply_validated_prepared_document_rewrite(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
) -> Result<DocId, SQLError> {
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
pub(in crate::sql) fn lock_referencing_child(
    engine: &Engine,
    ref_table: &str,
    child: &PhysicalDocumentIdentity,
    lock_columns: &[String],
    fk: &ForeignKey,
    comparison: &ForeignKeyComparison,
    expected: &[Value],
) -> Result<Option<(PhysicalDocumentIdentity, Document)>, SQLError> {
    let target = lock_physical_mutation_target(
        engine,
        &child.table,
        ref_table,
        child.doc_id,
        update_lock_strength(engine, &child.table, lock_columns),
    )?;
    let PhysicalMutationLockTarget::Present { identity, recheck } = target else {
        return Ok(None);
    };
    if recheck {
        engine.refresh_explicit_statement_snapshot()?;
    }
    let Some(child_doc) = engine.get_document_for_mutation(&identity.table, identity.doc_id)?
    else {
        return Ok(None);
    };
    let actual = fk
        .local_columns
        .iter()
        .map(|column| child_doc.get(column).cloned().unwrap_or(Value::Null))
        .collect();
    let actual = comparison.normalize(actual)?;
    Ok((actual == expected).then_some((identity, child_doc)))
}

pub(in crate::sql) fn referencing_rows(
    engine: &Engine,
    table: &str,
    fk: &ForeignKey,
    comparison: &ForeignKeyComparison,
    expected: &[Value],
) -> Result<Vec<(PhysicalDocumentIdentity, Document)>, SQLError> {
    let mut out = Vec::new();
    for physical_table in engine.hierarchy_scan_tables(table, true)? {
        for doc_id in engine.table_doc_ids(&physical_table)? {
            let Some(doc) = engine.get_document(&physical_table, doc_id)? else {
                return Err(missing_document_error(
                    "foreign-key reference scan",
                    &physical_table,
                    doc_id,
                ));
            };
            let values = fk
                .local_columns
                .iter()
                .map(|column| doc.get(column).cloned().unwrap_or(Value::Null))
                .collect();
            if comparison.normalize(values)? == expected {
                out.push((
                    PhysicalDocumentIdentity {
                        table: physical_table.clone(),
                        doc_id,
                    },
                    doc,
                ));
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
