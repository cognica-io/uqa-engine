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
            let Some(values) = foreign_key_values(&validation.foreign_key, &document, comparison)?
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
        return Err(SQLError::Routine {
            sqlstate: "23503".into(),
            message: format!(
                "insert or update on table \"{}\" violates foreign key constraint \"{}\"",
                foreign_key_relation_name(&table),
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

fn prepare_deferred_foreign_key(
    context: ConstraintContext<'_>,
    table: &str,
    constraint: &uqa_sql::catalog::constraints::ConstraintIdentity,
) -> Result<Option<DeferredForeignKeyValidation>, SQLError> {
    let constraint_table = constraint.relation.qualified_name();
    let foreign_key = context
        .catalog
        .try_foreign_keys(&constraint_table)
        .map_err(|error| dml_storage_error("deferred constraint validation", error))?
        .into_iter()
        .find(|foreign_key| {
            foreign_key.name.as_deref() == Some(&constraint.name)
                && foreign_key.object_id == constraint.object_id
        });
    let validation =
        if let Some(foreign_key) = foreign_key.filter(|foreign_key| foreign_key.enforced) {
            if foreign_key.period {
                Some(DeferredForeignKeyValidation {
                    foreign_key,
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
                    comparison: Some(comparison),
                    cross_type_parent_keys,
                })
            }
        } else {
            None
        };
    Ok(validation)
}
