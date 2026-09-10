//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unique-index access-method and partition-key declaration rules.
use crate::{
    ast::{CreateIndex, TableHierarchy},
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
    if let Some(partition) = &hierarchy.partition_spec {
        for key in &partition.keys {
            if !matches!(key, crate::ast::Expr::Column(column) if statement.columns.iter().any(|key| key.column() == Some(column.as_str())))
            {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "unique constraint on partitioned table must include all partitioning columns".into(),
                });
            }
        }
    }
    Ok(())
}
