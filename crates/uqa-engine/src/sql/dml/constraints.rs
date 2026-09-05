//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row, key, foreign-key, and referential-action validation.

mod index_keys;
mod period;

pub(in crate::sql) use index_keys::index_predicate_accepts;
mod referential_actions;
mod staging;

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
pub(in crate::sql) use referential_actions::prepare_referenced_key_delete_actions;
pub(in crate::sql) use staging::{
    stage_prepared_document_rewrite, stage_prepared_document_rewrite_with_parent,
};

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

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
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

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
fn lock_document_foreign_key_dependencies(
    engine: &Engine,
    table: &str,
    document: &Document,
    allow_missing: bool,
    old_document: Option<&Document>,
) -> Result<(), SQLError> {
    if engine.session_replication_role_is_replica() {
        return Ok(());
    }
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

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
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
        .enforced_keys(&canonical_table)
        .map_err(|error| dml_storage_error("key-lock constraint lookup", error))?;
    let mut lock_keys = std::collections::BTreeSet::new();
    for constraint in constraints {
        let Some(values) = constraint.values(engine, table, document)? else {
            continue;
        };
        if old_document
            .map(|old_document| constraint.values(engine, table, old_document))
            .transpose()?
            .flatten()
            .as_ref()
            == Some(&values)
        {
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

    let has_reservations = !lock_keys.is_empty();
    let mut acquisitions = Vec::new();
    for lock_key in lock_keys {
        match engine.lock_key_reservation(lock_key, table)? {
            crate::row_locks::LockAcquire::Granted { acquisition, .. } => {
                acquisitions.extend(acquisition);
            }
            crate::row_locks::LockAcquire::Skipped => {
                return Err(SQLError::Internal(
                    "blocking key reservation unexpectedly skipped a key".into(),
                ));
            }
        }
    }
    if has_reservations {
        // A competing writer can publish and release this reservation after our initial snapshot but immediately before acquisition. That grant does not report a wait, so every reservation boundary must refresh the READ COMMITTED snapshot before conflict lookup.
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
        .enforced_keys(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        let Some(values) = constraint.values(engine, table, document)? else {
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
        let Some(conflict_id) = constraint.find_conflict(engine, table, &values, ignored_doc_id)?
        else {
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
    authorize_foreign_key_parent_namespace(engine, fk)?;
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

pub(super) fn authorize_foreign_key_parent_namespace(
    engine: &Engine,
    foreign_key: &ForeignKey,
) -> Result<(), SQLError> {
    let relation =
        crate::RelationIdentity::from_legacy_name(&foreign_key.ref_table).map_err(|error| {
            SQLError::Internal(format!(
                "decode stored FOREIGN KEY relation `{}`: {error}",
                foreign_key.ref_table
            ))
        })?;
    engine.require_schema_privilege(
        &relation.schema,
        &engine.current_user_name(),
        crate::engine_schema_security::SchemaAclPrivilege::Usage,
    )
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

pub(in crate::sql) enum PartitionUpdateRoute {
    Rewrite {
        document: Document,
        destination: Option<String>,
    },
    Delete {
        attempted_document: Document,
    },
}

mod referencing;
mod rewrite;
pub(in crate::sql) use referencing::{
    apply_set_action_to_child, integer_primary_key_doc_id, referencing_rows,
    referrers_to_for_actions, ReferencingChildLock,
};
pub(in crate::sql) use rewrite::{
    apply_validated_prepared_document_rewrite, prepare_document_rewrite,
    prepare_partition_update_route, prepare_referential_document_rewrite,
    prepare_routed_document_rewrite, reject_partition_rewrite,
};
