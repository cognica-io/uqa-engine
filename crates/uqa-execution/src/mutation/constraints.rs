//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical CHECK, key, and foreign-key enforcement with transaction-scoped locking.
pub mod context;
mod deferred;
pub mod index_keys;
mod keys;
pub mod period;
use crate::mutation::{
    candidate::{MutationLockTarget, PhysicalDocumentIdentity},
    errors::{dml_storage_error, missing_document_error},
    locking::lock_mutation_target,
};
pub use context::ConstraintContext;
pub use deferred::validate_deferred_foreign_key_checks;
use index_keys::EnforcedKeyExecution;
pub use keys::{
    lock_document_key_dependencies, validate_key_constraints, without_overlaps_conflict,
};
use period::period_foreign_key_coverage;
use uqa_core::{DocId, Value};
use uqa_sql::{
    ast::{ForeignKey, TableKeyConstraint},
    semantics::foreign_keys::{
        foreign_key_comparison_types, foreign_key_lookup_values, foreign_key_parent_values,
        foreign_key_relation_name, foreign_key_values, ForeignKeyComparison, ForeignKeyLookup,
    },
    SQLError, SQLParam,
};
use uqa_storage::document_store::Document;

pub fn validate_document_constraints(
    context: ConstraintContext<'_>,
    table: &str,
    document: &Document,
    params: &[SQLParam],
    ignored_doc_id: Option<DocId>,
) -> Result<(), SQLError> {
    validate_document_non_key_constraints(context, table, document, params)?;
    validate_key_constraints(context, table, document, ignored_doc_id)
}

pub fn validate_document_rewrite_constraints(
    context: ConstraintContext<'_>,
    table: &str,
    old_document: &Document,
    new_document: &Document,
    params: &[SQLParam],
    doc_id: DocId,
) -> Result<(), SQLError> {
    validate_document_non_key_constraints_with_old(
        context,
        table,
        new_document,
        params,
        Some(old_document),
    )?;
    validate_key_constraints(context, table, new_document, Some(doc_id))
}

pub fn validate_document_non_key_constraints(
    context: ConstraintContext<'_>,
    table: &str,
    document: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    validate_document_non_key_constraints_with_old(context, table, document, params, None)
}

fn validate_document_non_key_constraints_with_old(
    context: ConstraintContext<'_>,
    table: &str,
    document: &Document,
    params: &[SQLParam],
    old_document: Option<&Document>,
) -> Result<(), SQLError> {
    let definitions = context
        .catalog
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let check_constraints = context
        .catalog
        .try_check_constraint_definitions(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?;
    let schema = crate::RowSchema::with_types(
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
                    && uqa_sql::schema::dependencies::schema_expr_references_column(
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
        crate::query::generated::materialize_selected_virtual_generated_columns(
            &definitions,
            &mut logical_document,
            &required_virtual_columns,
        )?;
        Some(logical_document)
    };
    let document = logical_document.as_ref().unwrap_or(document);

    validate_not_null_columns(&definitions, table, document)?;

    for constraint in check_constraints {
        if !constraint.enforced {
            continue;
        }
        let accepted = if let Some(partition) = constraint.partition_constraint.as_ref() {
            uqa_sql::semantics::partition::partition_constraint_accepts_document(
                &context.partitions,
                table,
                &partition.spec,
                &partition.bound,
                document,
            )?
        } else {
            let result = context.partitions.expressions.evaluate_row(
                &constraint.expr,
                document,
                &schema,
                params,
            )?;
            matches!(result, Value::Null) || uqa_sql::expr::truthy(&result)
        };
        if !accepted {
            let label = constraint.name.unwrap_or_else(|| "<unnamed>".into());
            let relation = uqa_core::RelationIdentity::from_legacy_name(table)
                .map_or_else(|_| table.to_string(), |identity| identity.name);
            return Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: format!(
                    "new row for relation \"{relation}\" violates check constraint \"{label}\""
                ),
            });
        }
    }

    lock_document_foreign_key_dependencies(context, table, document, false, old_document)
}

pub fn lock_existing_document_foreign_key_dependencies(
    context: ConstraintContext<'_>,
    table: &str,
    document: &Document,
) -> Result<(), SQLError> {
    lock_document_foreign_key_dependencies(context, table, document, true, None)
}

pub fn lock_existing_document_rewrite_foreign_key_dependencies(
    context: ConstraintContext<'_>,
    table: &str,
    old_document: &Document,
    new_document: &Document,
) -> Result<(), SQLError> {
    lock_document_foreign_key_dependencies(context, table, new_document, true, Some(old_document))
}

fn lock_document_foreign_key_dependencies(
    context: ConstraintContext<'_>,
    table: &str,
    document: &Document,
    allow_missing: bool,
    old_document: Option<&Document>,
) -> Result<(), SQLError> {
    if context.referrers.session_replication_role_is_replica() {
        return Ok(());
    }
    for fk in context
        .catalog
        .try_foreign_keys(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        if !fk.enforced {
            continue;
        }
        if !allow_missing && context.transactions.foreign_key_is_deferred(table, &fk)? {
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
        let Some(local_values) =
            foreign_key_lookup_values(context.partitions.catalog, table, &fk, document)?
        else {
            continue;
        };
        let violation = || SQLError::Routine {
            sqlstate: "23503".into(),
            message: format!(
                "insert or update on table \"{}\" violates foreign key constraint \"{}\"",
                foreign_key_relation_name(table),
                fk.name.as_deref().unwrap_or("<unnamed>")
            ),
        };
        if fk.period {
            let (covered, parent_ids) =
                period_foreign_key_coverage(context, &fk, &local_values.values, &[], None)?;
            if !covered {
                if allow_missing {
                    continue;
                }
                return Err(violation());
            }
            for parent in parent_ids {
                let _target = lock_mutation_target(
                    context.locks,
                    &parent.table,
                    &fk.ref_table,
                    parent.doc_id,
                    uqa_sql::ast::LockStrength::ForKeyShare,
                )?;
            }
            continue;
        }
        lock_foreign_key_parent(context, table, &fk, &local_values, allow_missing)?;
    }
    Ok(())
}

pub fn find_foreign_key_parent(
    context: ConstraintContext<'_>,
    fk: &ForeignKey,
    lookup: &ForeignKeyLookup,
) -> Result<Option<PhysicalDocumentIdentity>, SQLError> {
    authorize_foreign_key_parent_namespace(context, fk)?;
    if lookup.comparison.exact_reference_lookup {
        return find_exact_foreign_key_parent(context, fk, &lookup.values);
    }
    for physical_table in context.catalog.hierarchy_scan_tables(&fk.ref_table, true)? {
        for doc_id in context.reads.table_doc_ids(&physical_table)? {
            let Some(document) = context.reads.get_document(&physical_table, doc_id)? else {
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

pub fn authorize_foreign_key_parent_namespace(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
) -> Result<(), SQLError> {
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(&foreign_key.ref_table).map_err(|error| {
            SQLError::Internal(format!(
                "decode stored FOREIGN KEY relation `{}`: {error}",
                foreign_key.ref_table
            ))
        })?;
    context.namespace.require_schema_privilege(
        &relation.schema,
        &context.namespace.current_user_name(),
        crate::catalog::security::schema::SchemaAclPrivilege::Usage,
    )
}

fn find_exact_foreign_key_parent(
    context: ConstraintContext<'_>,
    fk: &ForeignKey,
    values: &[Value],
) -> Result<Option<PhysicalDocumentIdentity>, SQLError> {
    for physical_table in context.catalog.hierarchy_scan_tables(&fk.ref_table, true)? {
        if let Some(doc_id) =
            context
                .indexes
                .find_conflict(&physical_table, &fk.ref_columns, values)?
        {
            return Ok(Some(PhysicalDocumentIdentity {
                table: physical_table,
                doc_id,
            }));
        }
    }
    Ok(None)
}

fn foreign_key_parent_index(
    context: ConstraintContext<'_>,
    fk: &ForeignKey,
    comparison: &ForeignKeyComparison,
) -> Result<std::collections::BTreeSet<Vec<Value>>, SQLError> {
    let mut keys = std::collections::BTreeSet::new();
    for physical_table in context.catalog.hierarchy_scan_tables(&fk.ref_table, true)? {
        for doc_id in context.reads.table_doc_ids(&physical_table)? {
            let Some(document) = context.reads.get_document(&physical_table, doc_id)? else {
                continue;
            };
            keys.insert(foreign_key_parent_values(fk, &document, comparison)?);
        }
    }
    Ok(keys)
}

fn validate_not_null_columns(
    definitions: &[uqa_sql::ast::ColumnDef],
    table: &str,
    document: &Document,
) -> Result<(), SQLError> {
    for col_def in definitions {
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

    Ok(())
}

fn lock_foreign_key_parent(
    context: ConstraintContext<'_>,
    table: &str,
    fk: &ForeignKey,
    local_values: &ForeignKeyLookup,
    allow_missing: bool,
) -> Result<(), SQLError> {
    let violation = || SQLError::Routine {
        sqlstate: "23503".into(),
        message: format!(
            "insert or update on table \"{}\" violates foreign key constraint \"{}\"",
            foreign_key_relation_name(table),
            fk.name.as_deref().unwrap_or("<unnamed>")
        ),
    };
    let mut hops = 0usize;
    loop {
        let Some(parent) = find_foreign_key_parent(context, fk, local_values)? else {
            if allow_missing {
                break;
            }
            return Err(violation());
        };
        // PostgreSQL 18 holds FOR KEY SHARE on the referenced row until the referencing transaction ends. If the lookup waits, refresh the READ COMMITTED snapshot and follow a delete/reinsert or key rewrite until the tuple carrying the requested key is locked.
        let target = lock_mutation_target(
            context.locks,
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
            context.transactions.refresh_explicit_statement_snapshot()?;
            hops += 1;
            if hops > 64 {
                return Err(SQLError::Internal(format!(
                    "foreign-key parent lookup for `{table}` did not converge"
                )));
            }
            continue;
        };
        if recheck {
            context.transactions.refresh_explicit_statement_snapshot()?;
        }
        let locked_parent = PhysicalDocumentIdentity {
            table: parent.table,
            doc_id: locked_parent,
        };
        match find_foreign_key_parent(context, fk, local_values)? {
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
    Ok(())
}
