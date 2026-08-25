//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE inheritance and partition row-type preparation.

use super::{CreateTable, Engine, SQLError};
use std::collections::BTreeSet;

pub(super) fn prepare_create_table_hierarchy(
    engine: &Engine,
    table: &mut CreateTable,
) -> Result<(), SQLError> {
    table.hierarchy.local_columns = table
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect();
    if table.hierarchy.parents.is_empty() {
        if table.hierarchy.partition_bound.is_some() {
            return Err(SQLError::Internal(
                "partition bound has no parent relation".into(),
            ));
        }
        validate_partition_keys(engine, table)?;
        return Ok(());
    }
    let is_partition = table.hierarchy.partition_bound.is_some();
    if is_partition && table.hierarchy.parents.len() != 1 {
        return Err(SQLError::Internal(
            "a partition must have exactly one parent".into(),
        ));
    }
    let mut canonical_parents = Vec::with_capacity(table.hierarchy.parents.len());
    let mut inherited_columns = Vec::new();
    let mut inherited_checks = Vec::new();
    let mut inherited_foreign_keys = Vec::new();
    let mut inherited_keys = Vec::new();
    for requested_parent in &table.hierarchy.parents {
        let parent = engine
            .try_resolve_table_name(requested_parent)
            .map_err(|error| SQLError::Internal(format!("resolve inherited table: {error}")))?
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{requested_parent}\" does not exist"),
            })?;
        if parent == table.name {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: "circular inheritance not allowed".into(),
            });
        }
        let parent_hierarchy = engine
            .try_table_hierarchy(&parent)
            .map_err(|error| SQLError::Internal(format!("read parent hierarchy: {error}")))?;
        if is_partition {
            let Some(parent_spec) = parent_hierarchy.partition_spec.as_ref() else {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("relation \"{requested_parent}\" is not partitioned"),
                });
            };
            validate_partition_bound_strategy(
                parent_spec.strategy,
                table.hierarchy.partition_bound.as_ref().ok_or_else(|| {
                    SQLError::Internal("partition lost its bound during validation".into())
                })?,
            )?;
        } else if parent_hierarchy.partition_spec.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot inherit from partitioned table \"{requested_parent}\""),
            });
        }
        let mut columns = engine
            .try_describe_table(&parent)
            .map_err(|error| SQLError::Internal(format!("read inherited row type: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(parent.clone()))?;
        if !is_partition {
            // PostgreSQL inherits the NOT NULL property of an identity column, but not its identity generation attribute or owned sequence. SERIAL is different: its nextval default is ordinary inherited metadata and therefore keeps pointing at the parent's sequence.
            for column in &mut columns {
                if column
                    .auto_increment
                    .as_ref()
                    .is_some_and(uqa_sql::ast::AutoIncrement::is_identity)
                {
                    column.auto_increment = None;
                }
            }
        }
        merge_parent_columns(&mut inherited_columns, columns)?;
        let constraints = engine
            .try_declared_table_constraints(&parent)
            .map_err(|error| SQLError::Internal(format!("read inherited constraints: {error}")))?;
        inherited_checks.extend(
            constraints
                .checks
                .into_iter()
                .filter(|constraint| !constraint.no_inherit),
        );
        if is_partition {
            inherited_foreign_keys.extend(constraints.foreign_keys);
            inherited_keys.extend(constraints.key_constraints);
        }
        canonical_parents.push(parent);
    }
    merge_local_columns(&mut inherited_columns, std::mem::take(&mut table.columns))?;
    table.columns = inherited_columns;
    inherited_checks.append(&mut table.checks);
    table.checks = inherited_checks;
    if is_partition {
        inherited_foreign_keys.append(&mut table.foreign_keys);
        inherited_keys.append(&mut table.key_constraints);
        table.foreign_keys = inherited_foreign_keys;
        table.key_constraints = inherited_keys;
    }
    table.hierarchy.parents = canonical_parents;
    validate_partition_keys(engine, table)?;
    if let (Some(parent), Some(bound)) = (
        table.hierarchy.parents.first(),
        table.hierarchy.partition_bound.as_ref(),
    ) {
        crate::sql::validate_new_partition_bound(engine, parent, bound)?;
    }
    Ok(())
}

fn validate_partition_bound_strategy(
    strategy: uqa_sql::ast::PartitionStrategy,
    bound: &uqa_sql::ast::PartitionBound,
) -> Result<(), SQLError> {
    use uqa_sql::ast::{PartitionBound, PartitionStrategy};
    if matches!(
        (strategy, bound),
        (PartitionStrategy::Hash, PartitionBound::Default)
    ) {
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: "a hash-partitioned table may not have a default partition".into(),
        });
    }
    let matches = matches!(bound, PartitionBound::Default)
        || matches!(
            (strategy, bound),
            (PartitionStrategy::List, PartitionBound::List(_))
                | (PartitionStrategy::Range, PartitionBound::Range { .. })
                | (PartitionStrategy::Hash, PartitionBound::Hash { .. })
        );
    if matches {
        Ok(())
    } else {
        Err(SQLError::Internal(
            "partition bound strategy differs from its parent".into(),
        ))
    }
}

fn merge_parent_columns(
    merged: &mut Vec<uqa_sql::ast::ColumnDef>,
    incoming: Vec<uqa_sql::ast::ColumnDef>,
) -> Result<(), SQLError> {
    for column in incoming {
        if let Some(existing) = merged.iter_mut().find(|item| item.name == column.name) {
            merge_same_column(existing, column)?;
        } else {
            merged.push(column);
        }
    }
    Ok(())
}

fn merge_local_columns(
    inherited: &mut Vec<uqa_sql::ast::ColumnDef>,
    local: Vec<uqa_sql::ast::ColumnDef>,
) -> Result<(), SQLError> {
    for column in local {
        if let Some(existing) = inherited.iter_mut().find(|item| item.name == column.name) {
            merge_same_column(existing, column)?;
        } else {
            inherited.push(column);
        }
    }
    Ok(())
}

pub(super) fn merge_same_column(
    inherited: &mut uqa_sql::ast::ColumnDef,
    declared: uqa_sql::ast::ColumnDef,
) -> Result<(), SQLError> {
    if inherited.ty != declared.ty {
        return Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: format!(
                "inherited column \"{}\" has a type conflict",
                inherited.name
            ),
        });
    }
    if inherited.generated.is_some() != declared.generated.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: format!(
                "inherited column \"{}\" has a generation conflict",
                inherited.name
            ),
        });
    }
    inherited.not_null |= declared.not_null;
    inherited.not_null_explicit |= declared.not_null_explicit;
    inherited.primary_key |= declared.primary_key;
    inherited.unique |= declared.unique;
    if declared.auto_increment.is_some() {
        inherited.auto_increment = declared.auto_increment;
    }
    if declared.default.is_some() {
        inherited.default = declared.default;
    }
    if declared.generated.is_some() {
        inherited.generated = declared.generated;
    }
    if declared.check.is_some() {
        inherited.check = declared.check;
        inherited.check_name = declared.check_name;
        inherited.check_enforced = declared.check_enforced;
    }
    if declared.references.is_some() {
        inherited.references = declared.references;
    }
    Ok(())
}

fn validate_partition_keys(engine: &Engine, table: &CreateTable) -> Result<(), SQLError> {
    let Some(spec) = table.hierarchy.partition_spec.as_ref() else {
        return Ok(());
    };
    let column_names = table
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect::<BTreeSet<_>>();
    for key in &spec.keys {
        if let uqa_sql::ast::Expr::Column(column) = key {
            if !column_names.contains(column.as_str()) {
                return Err(SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!("column \"{column}\" named in partition key does not exist"),
                });
            }
        }
    }
    crate::sql::validate_hash_partition_spec(engine, spec, &table.columns)
}
