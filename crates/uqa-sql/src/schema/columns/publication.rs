//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutate a declared column candidate before its catalog publication.
use crate::ast::{ColumnDef, ColumnType, Expr, GeneratedColumn};

pub enum ColumnProperty<'a> {
    Default(Option<Expr>),
    Generated(Option<GeneratedColumn>),
    Type(&'a ColumnType),
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
    }
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
    }
    Ok(())
}
