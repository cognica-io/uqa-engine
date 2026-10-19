//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` primary-key NOT NULL preparation and generated constraint targets.

use crate::{
    ast::{ColumnDef, TableKeyConstraint, TableKeyConstraintKind},
    SQLError,
};

pub fn validate_primary_key_not_null_constraints(
    table: &str,
    columns: &[ColumnDef],
    key: &TableKeyConstraint,
) -> Result<(), SQLError> {
    if key.kind == TableKeyConstraintKind::PrimaryKey {
        for name in &key.columns {
            if let Some(column) = columns
                .iter()
                .find(|column| column.name == *name && column.not_null)
            {
                validate_primary_key_not_null_constraint(table, column)?;
            }
        }
    }
    Ok(())
}

pub fn validate_primary_key_not_null_constraint(
    table: &str,
    column: &ColumnDef,
) -> Result<(), SQLError> {
    let (property, hint) = if column.not_null_no_inherit {
        (
            "NO INHERIT",
            "You might need to make the existing constraint inheritable using ALTER TABLE ... ALTER CONSTRAINT ... INHERIT.",
        )
    } else if !column.not_null_validated {
        (
            "NOT VALID",
            "You might need to validate it using ALTER TABLE ... VALIDATE CONSTRAINT.",
        )
    } else {
        return Ok(());
    };
    let constraint = column
        .not_null_name
        .as_deref()
        .ok_or_else(|| SQLError::Internal("existing NOT NULL constraint has no name".into()))?;
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    let name = &column.name;
    Err(SQLError::Diagnostic {
        sqlstate: "55000".into(),
        message: format!("cannot create primary key on column \"{name}\""),
        detail: Some(format!("The constraint \"{constraint}\" on column \"{name}\" of table \"{}\", marked {property}, is incompatible with a primary key.", relation.name)),
        hint: Some(hint.into()),
    })
}

pub fn validate_primary_key_not_null_child(
    table: &str,
    columns: &[ColumnDef],
    name: &str,
) -> Result<(), SQLError> {
    let column = columns
        .iter()
        .find(|column| column.name == name && column.not_null);
    let Some(column) = column else {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
        return Err(SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!(
                "column \"{name}\" of table \"{}\" is not marked NOT NULL",
                relation.name
            ),
        });
    };
    validate_primary_key_not_null_constraint(table, column)
}

pub fn validate_primary_key_not_null_target(
    table: &str,
    columns: &[ColumnDef],
    name: &str,
) -> Result<(), SQLError> {
    if columns.iter().any(|column| column.name == name) {
        return Ok(());
    }
    if super::definition::is_system_column(name) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("cannot add not-null constraint on system column \"{name}\""),
        });
    }
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    Err(SQLError::Routine {
        sqlstate: "42703".into(),
        message: format!(
            "column \"{name}\" of relation \"{}\" does not exist",
            relation.name
        ),
    })
}
