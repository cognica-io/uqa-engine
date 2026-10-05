//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered checks of an ALTER COLUMN TYPE target before its new type is resolved.

use crate::ast::{ColumnDef, ColumnType, PartitionSpec};
use crate::SQLError;

/// Check the target in `ATPrepAlterColumnType` order after the USING expression has been analyzed. `inherited` says that a parent supplies this column and the request directly alters this relation; recursive changes initiated at a parent pass false after their inheritance checks. `partition` is this relation's already-validated partition key. The returned definition belongs to the original, unchanged row type.
pub fn validate_type_target<'a>(
    table: &str,
    columns: &'a [ColumnDef],
    name: &str,
    has_using: bool,
    inherited: bool,
    partition: Option<&PartitionSpec>,
) -> Result<&'a ColumnDef, SQLError> {
    let column = super::altered_column(table, columns, name)?;
    if has_using && column.generated.is_some() {
        return Err(SQLError::Diagnostic {
            sqlstate: "42611".into(),
            message: "cannot specify USING when altering type of generated column".into(),
            detail: Some(format!("Column \"{name}\" is a generated column.")),
            hint: None,
        });
    }
    if inherited {
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: format!("cannot alter inherited column \"{name}\""),
        });
    }
    if partition.is_some_and(|spec| {
        spec.keys
            .iter()
            .any(|key| crate::schema::dependencies::schema_expr_references_column(key, name))
    }) {
        let relation = crate::RelationIdentity::from_legacy_name(table).map_err(|error| {
            SQLError::Internal(format!("resolve ALTER TABLE target `{table}`: {error}"))
        })?;
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: format!(
                "cannot alter column \"{name}\" because it is part of the partition key of relation \"{}\"",
                relation.name
            ),
        });
    }
    Ok(column)
}

/// The executor may repeat a same-type transform, but cannot change an attribute's type or modifier twice from its original declaration.
pub fn validate_repeated_type_change(
    name: &str,
    original: &ColumnType,
    current: &ColumnType,
) -> Result<(), SQLError> {
    if original != current {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("cannot alter type of column \"{name}\" twice"),
        });
    }
    Ok(())
}

pub fn require_type_change_descendants(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P16".into(),
        message: format!("type of inherited column \"{name}\" must be changed in child tables too"),
    }
}

pub fn reject_external_type_parent(table: &str, name: &str) -> SQLError {
    let (_, relation) = uqa_core::RelationIdentity::parse_reference(table)
        .unwrap_or_else(|_| (None, table.to_string()));
    SQLError::Routine {
        sqlstate: "42P16".into(),
        message: format!("cannot alter inherited column \"{name}\" of relation \"{relation}\""),
    }
}

#[cfg(test)]
mod tests;
