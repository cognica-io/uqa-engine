//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutate a declared column candidate before its catalog publication.
use crate::ast::{ColumnDef, ColumnType, Expr, GeneratedColumn};

/// Rename the declaration without changing its durable column identity or attribute number.
pub fn rename_column(columns: &mut [ColumnDef], from: &str, to: &str) {
    for column in columns {
        if column.name == from {
            column.name = to.to_string();
        }
    }
}

/// Record a newly declared local column; recursively added parent columns retain their inherited origin.
pub fn register_local_column(
    hierarchy: &mut crate::ast::TableHierarchy,
    name: &str,
    inherited: bool,
) {
    if !inherited && !hierarchy.local_columns.iter().any(|column| column == name) {
        hierarchy.local_columns.push(name.to_string());
    }
}

/// A rename changes the spelling of the local declaration, not its origin.
pub fn rename_local_column(hierarchy: &mut crate::ast::TableHierarchy, from: &str, to: &str) {
    for name in &mut hierarchy.local_columns {
        if name == from {
            *name = to.to_string();
        }
    }
}

/// Removing a local declaration lets a later same-named inherited column have its own origin.
pub fn remove_local_column(hierarchy: &mut crate::ast::TableHierarchy, name: &str) {
    hierarchy.local_columns.retain(|column| column != name);
}

#[derive(Clone)]
pub enum ColumnProperty<'a> {
    Default(Option<Expr>),
    Generated(Option<GeneratedColumn>),
    Type(&'a ColumnType),
    /// The `SERIAL` or identity provenance of the column.
    AutoIncrement(Option<crate::ast::AutoIncrement>),
}
pub fn column_mut<'a>(
    columns: &'a mut [ColumnDef],
    table: &str,
    column: &str,
) -> Result<&'a mut ColumnDef, String> {
    columns
        .iter_mut()
        .find(|definition| definition.name == column)
        .ok_or_else(|| format!("column `{column}` does not exist on table `{table}`"))
}
pub fn apply_property(
    columns: &mut [ColumnDef],
    table: &str,
    column: &str,
    property: ColumnProperty<'_>,
) -> Result<(), String> {
    let definition = column_mut(columns, table, column)?;
    match property {
        ColumnProperty::Default(default) => definition.default = default,
        ColumnProperty::Generated(generated) => definition.generated = generated,
        ColumnProperty::Type(ty) => definition.ty.clone_from(ty),
        ColumnProperty::AutoIncrement(provenance) => definition.auto_increment = provenance,
    }
    // Each change removes the column's `pg_attrdef` row; an expression that remains is stored again under a new OID, as `ATExecAlterColumnType` and `ATExecColumnDefault` store it.
    definition.default_catalog_oid = None;
    Ok(())
}
pub fn set_not_null(
    columns: &mut [ColumnDef],
    table: &str,
    column: &str,
    not_null: bool,
) -> Result<(), String> {
    let definition = column_mut(columns, table, column)?;
    definition.not_null = not_null;
    definition.not_null_explicit = not_null;
    definition.not_null_is_local = true;
    definition.not_null_validated = true;
    definition.not_null_no_inherit = false;
    if !not_null {
        definition.not_null_name = None;
        definition.not_null_identity = None;
    }
    Ok(())
}
