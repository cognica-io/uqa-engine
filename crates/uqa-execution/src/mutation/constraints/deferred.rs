//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate deferred references against the rows visible at the transaction boundary.
use super::{
    dml_storage_error, find_exact_foreign_key_parent, foreign_key_comparison_types,
    foreign_key_lookup_values, foreign_key_parent_index, foreign_key_relation_name,
    foreign_key_values, period_foreign_key_coverage, ConstraintContext, ForeignKey,
    ForeignKeyComparison, SQLError, Value,
};

struct DeferredForeignKeyValidation {
    foreign_key: ForeignKey,
    /// The current name of the deferred constraint: the foreign key's, or the derived constraint's whose partition fired the check.
    constraint_name: String,
    comparison: Option<ForeignKeyComparison>,
    cross_type_parent_keys: Option<std::collections::BTreeSet<Vec<Value>>>,
}

pub fn validate_deferred_foreign_key_checks(
    context: ConstraintContext<'_>,
    checks: &[crate::mutation::deferred::DeferredForeignKeyCheck],
    targets: Option<&std::collections::BTreeSet<uqa_sql::catalog::constraints::ConstraintIdentity>>,
) -> Result<(), SQLError> {
    let mut validation_by_constraint = std::collections::BTreeMap::new();
    for check in checks {
        let selected = targets.is_none_or(|targets| {
            targets.iter().any(|target| {
                uqa_sql::catalog::constraints::constraint_identities_match(
                    target,
                    &check.constraint,
                )
            })
        });
        if !selected {
            continue;
        }
        let Some(row) = check.row else {
            continue;
        };
        let table = context
            .locks
            .lock_manager()
            .table_name(row.table)
            .to_string();
        let cache_key = (check.constraint.clone(), table.clone());
        if !validation_by_constraint.contains_key(&cache_key) {
            let validation = prepare_deferred_foreign_key(context, &table, &check.constraint)?;
            validation_by_constraint.insert(cache_key.clone(), validation);
        }
        let Some(validation) = validation_by_constraint
            .get(&cache_key)
            .and_then(Option::as_ref)
        else {
            continue;
        };
        let Some(document) = context.reads.get_document(&table, row.doc_id)? else {
            continue;
        };
        if validation.foreign_key.period {
            let Some(lookup) = foreign_key_lookup_values(
                context.partitions.catalog,
                &table,
                &validation.foreign_key,
                &document,
            )?
            else {
                continue;
            };
            if period_foreign_key_coverage(
                context,
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
            let Some(values) =
                foreign_key_values(&table, &validation.foreign_key, &document, comparison)?
            else {
                continue;
            };
            let parent_exists = if comparison.exact_reference_lookup {
                find_exact_foreign_key_parent(context, &validation.foreign_key, &values)?.is_some()
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
        return Err(deferred_violation(
            context, check, validation, &table, &document,
        )?);
    }
    Ok(())
}

/// `ri_ReportViolation` for a deferred check: a check that a change to a referenced row fired reports the referenced side with the key it still references, as `PostgreSQL`'s deferred `NO ACTION` triggers do, and a referencing row's check the key missing from the referenced table.
fn deferred_violation(
    context: ConstraintContext<'_>,
    check: &crate::mutation::deferred::DeferredForeignKeyCheck,
    validation: &DeferredForeignKeyValidation,
    table: &str,
    document: &uqa_storage::document_store::Document,
) -> Result<SQLError, SQLError> {
    let foreign_key = &validation.foreign_key;
    let values = foreign_key
        .local_columns
        .iter()
        .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
        .collect::<Vec<_>>();
    if check.referenced {
        let referencing = &check.constraint.relation.name;
        let key = super::foreign_key_key(
            context,
            &check.firing_relation.qualified_name(),
            &foreign_key.ref_columns,
            table,
            &foreign_key.local_columns,
            &values,
        )?;
        return Ok(SQLError::Diagnostic {
            sqlstate: "23503".into(),
            message: format!(
                "update or delete on table \"{}\" violates foreign key constraint \"{}\" on table \"{referencing}\"",
                check.firing_relation.name, validation.constraint_name
            ),
            detail: Some(match key {
                Some(key) => format!("{key} is still referenced from table \"{referencing}\"."),
                None => format!("Key is still referenced from table \"{referencing}\"."),
            }),
            hint: None,
        });
    }
    let referenced = foreign_key_relation_name(&foreign_key.ref_table);
    let key = super::foreign_key_key(
        context,
        table,
        &foreign_key.local_columns,
        table,
        &foreign_key.local_columns,
        &values,
    )?;
    Ok(SQLError::Diagnostic {
        sqlstate: "23503".into(),
        message: format!(
            "insert or update on table \"{}\" violates foreign key constraint \"{}\"",
            foreign_key_relation_name(table),
            validation.constraint_name
        ),
        detail: Some(match key {
            Some(key) => format!("{key} is not present in table \"{referenced}\"."),
            None => format!("Key is not present in table \"{referenced}\"."),
        }),
        hint: None,
    })
}

fn prepare_deferred_foreign_key(
    context: ConstraintContext<'_>,
    table: &str,
    constraint: &uqa_sql::catalog::constraints::ConstraintIdentity,
) -> Result<Option<DeferredForeignKeyValidation>, SQLError> {
    let constraint_table = constraint.relation.qualified_name();
    let foreign_keys = context
        .catalog
        .try_foreign_keys(&constraint_table)
        .map_err(|error| dml_storage_error("deferred constraint validation", error))?;
    // The constraint is the foreign key, or one it derives on a referenced partition.
    let found = foreign_keys
        .iter()
        .find(|foreign_key| {
            foreign_key.name.as_deref() == Some(&constraint.name)
                && foreign_key.object_id == constraint.object_id
        })
        .map(|foreign_key| {
            (
                foreign_key.clone(),
                foreign_key.name.clone().unwrap_or_default(),
            )
        })
        .or_else(|| {
            foreign_keys.iter().find_map(|foreign_key| {
                foreign_key
                    .referenced_partitions
                    .iter()
                    .find(|derived| {
                        Some(derived.catalog_identity.object_id) == constraint.object_id
                    })
                    .map(|derived| (foreign_key.clone(), derived.name.clone()))
            })
        });
    let validation = if let Some((foreign_key, constraint_name)) =
        found.filter(|(foreign_key, _)| foreign_key.enforced)
    {
        if foreign_key.period {
            Some(DeferredForeignKeyValidation {
                foreign_key,
                constraint_name,
                comparison: None,
                cross_type_parent_keys: None,
            })
        } else {
            let comparison =
                foreign_key_comparison_types(context.partitions.catalog, table, &foreign_key)?;
            let cross_type_parent_keys = if comparison.exact_reference_lookup {
                None
            } else {
                Some(foreign_key_parent_index(
                    context,
                    &foreign_key,
                    &comparison,
                )?)
            };
            Some(DeferredForeignKeyValidation {
                foreign_key,
                constraint_name,
                comparison: Some(comparison),
                cross_type_parent_keys,
            })
        }
    } else {
        None
    };
    Ok(validation)
}
