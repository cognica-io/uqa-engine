//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unique-index access-method and partition-key declaration rules.
use crate::{
    ast::{CreateIndex, PartitionSpec, TableHierarchy, TableKeyConstraint, TableKeyConstraintKind},
    SQLError,
};
pub fn validate_unique_index_method(statement: &CreateIndex) -> Result<(), SQLError> {
    if !matches!(statement.access_method.as_str(), "" | "btree") {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!(
                "access method \"{}\" does not support unique indexes",
                statement.access_method
            ),
        });
    }
    Ok(())
}
pub fn validate_unique_partition_columns(
    statement: &CreateIndex,
    hierarchy: &TableHierarchy,
) -> Result<(), SQLError> {
    let Some(partition) = &hierarchy.partition_spec else {
        return Ok(());
    };
    let columns = statement
        .columns
        .iter()
        .map(crate::ast::IndexKey::column)
        .collect::<Vec<_>>();
    validate_partitioned_unique_key(
        &statement.table,
        &PartitionedUniqueKey {
            constraint_type: TableKeyConstraintKind::Unique,
            columns: &columns,
            without_overlaps: false,
        },
        partition,
    )
}

/// Validate a key constraint of a partitioned table by its key columns; its `INCLUDE` columns take no part.
pub fn validate_partitioned_key_constraint(
    table: &str,
    constraint: &TableKeyConstraint,
    partition: &PartitionSpec,
) -> Result<(), SQLError> {
    let columns = constraint
        .columns
        .iter()
        .map(|column| Some(column.as_str()))
        .collect::<Vec<_>>();
    validate_partitioned_unique_key(
        table,
        &PartitionedUniqueKey {
            constraint_type: constraint.kind,
            columns: &columns,
            without_overlaps: constraint.without_overlaps,
        },
        partition,
    )
}

/// A unique key as `PostgreSQL`'s `DefineIndex` matches it against a partition key.
pub struct PartitionedUniqueKey<'a> {
    /// The constraint type the errors name; a unique index reports `UNIQUE`.
    pub constraint_type: TableKeyConstraintKind,
    /// The key columns in key order without `INCLUDE` columns, `None` for an expression key.
    pub columns: &'a [Option<&'a str>],
    /// The final key column is compared by overlap rather than equality.
    pub without_overlaps: bool,
}

/// Every partition key column must be compared by equality in a unique key of a partitioned table, so that each partition's index enforces the key alone. As `PostgreSQL`'s `DefineIndex` does, the partition key is scanned in order: an expression cannot be matched at all, a column must be one of the key columns, and the `WITHOUT OVERLAPS` column is compared by `&&` rather than equality.
pub fn validate_partitioned_unique_key(
    table: &str,
    key: &PartitionedUniqueKey<'_>,
    partition: &PartitionSpec,
) -> Result<(), SQLError> {
    let constraint_type = key.constraint_type.sql_label();
    for partition_key in &partition.keys {
        let crate::ast::Expr::Column(column) = partition_key else {
            return Err(SQLError::Diagnostic {
                sqlstate: "0A000".into(),
                message: format!(
                    "unsupported {constraint_type} constraint with partition key definition"
                ),
                detail: Some(format!(
                    "{constraint_type} constraints cannot be used when partition keys include expressions."
                )),
                hint: None,
            });
        };
        match key
            .columns
            .iter()
            .position(|candidate| *candidate == Some(column.as_str()))
        {
            Some(position) if key.without_overlaps && position + 1 == key.columns.len() => {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: format!(
                        "cannot match partition key to index on column \"{column}\" using non-equal operator \"&&\""
                    ),
                });
            }
            Some(_) => {}
            None => {
                let relation = uqa_core::RelationIdentity::from_legacy_name(table)
                    .map_err(SQLError::Internal)?;
                return Err(SQLError::Diagnostic {
                    sqlstate: "0A000".into(),
                    message: "unique constraint on partitioned table must include all partitioning columns".into(),
                    detail: Some(format!(
                        "{constraint_type} constraint on table \"{}\" lacks column \"{column}\" which is part of the partition key.",
                        relation.name
                    )),
                    hint: None,
                });
            }
        }
    }
    Ok(())
}
