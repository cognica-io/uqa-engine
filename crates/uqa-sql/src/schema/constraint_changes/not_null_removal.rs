//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Protect primary keys and identity columns when removing NOT NULL constraints.

use super::constraint_error;
use crate::{
    ast::{ColumnDef, TableConstraintSet, TableKeyConstraintKind},
    SQLError,
};

pub fn validate_constraint_removal(
    table: &str,
    column: &ColumnDef,
    constraints: &TableConstraintSet,
) -> Result<(), SQLError> {
    if column.primary_key
        || constraints.key_constraints.iter().any(|key| {
            key.kind == TableKeyConstraintKind::PrimaryKey && key.columns.contains(&column.name)
        })
    {
        return Err(constraint_error(
            "42P16",
            format!("column \"{}\" is in a primary key", column.name),
        ));
    }
    reject_identity(table, column, "55000")
}

pub fn validate_column_removal(table: &str, column: &ColumnDef) -> Result<(), SQLError> {
    if !column.not_null {
        return Ok(());
    }
    reject_identity(table, column, "42601")
}

fn reject_identity(table: &str, column: &ColumnDef, sqlstate: &str) -> Result<(), SQLError> {
    if !column
        .auto_increment
        .as_ref()
        .is_some_and(|definition| definition.is_identity())
    {
        return Ok(());
    }
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    Err(constraint_error(
        sqlstate,
        format!(
            "column \"{}\" of relation \"{}\" is an identity column",
            column.name, relation.name
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_not_null_removal_distinguishes_column_and_constraint_syntax() {
        let crate::Statement::CreateTable(table) = crate::compile(
            "CREATE TABLE t(v integer GENERATED ALWAYS AS IDENTITY CONSTRAINT nn NOT NULL)",
        )
        .unwrap()
        .remove(0) else {
            panic!("table");
        };
        let column = &table.columns[0];
        let constraints = TableConstraintSet::default();
        assert_eq!(
            validate_constraint_removal("public.t", column, &constraints)
                .unwrap_err()
                .sqlstate(),
            Some("55000")
        );
        assert_eq!(
            validate_column_removal("public.t", column)
                .unwrap_err()
                .sqlstate(),
            Some("42601")
        );
        let mut keyed = column.clone();
        keyed.primary_key = true;
        assert_eq!(
            validate_constraint_removal("public.t", &keyed, &constraints)
                .unwrap_err()
                .sqlstate(),
            Some("42P16")
        );
    }

    #[test]
    fn serial_and_nullable_columns_do_not_inherit_identity_protection() {
        let crate::Statement::CreateTable(table) =
            crate::compile("CREATE TABLE t(v serial CONSTRAINT nn NOT NULL, nullable integer)")
                .unwrap()
                .remove(0)
        else {
            panic!("table");
        };
        for column in &table.columns {
            validate_constraint_removal("public.t", column, &TableConstraintSet::default())
                .unwrap();
            validate_column_removal("public.t", column).unwrap();
        }
    }
}
