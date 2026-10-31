//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JOIN aliases publish either a canonical analysis descriptor or a physical layout over the original slots.

use crate::{RowSchema, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub fn join_alias_columns_with_control(
    schema: &RowSchema,
    alias: &str,
    column_aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    control.check()?;
    let available = schema.len();
    let specified = column_aliases.len();
    if specified > available {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message: format!("join expression \"{alias}\" has {available} columns available but {specified} columns specified"),
        });
    }
    let mut columns = ProductionVec::new(*control);
    for (position, column) in schema.columns().iter().enumerate() {
        let name = column_aliases.get(position).map_or_else(
            || schema.public_name(position).unwrap_or(column),
            String::as_str,
        );
        columns.push_produced(control.copy_text(name)?)?;
    }
    Ok(columns.finish()?)
}

pub fn alias_join_schema_with_control(
    schema: &RowSchema,
    alias: Option<&str>,
    column_aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    let Some(alias) = alias else {
        if column_aliases.is_empty() {
            return Ok(schema.clone());
        }
        return Err(SQLError::Internal(
            "JOIN column aliases exist without a relation alias".into(),
        ));
    };
    let columns = join_alias_columns_with_control(schema, alias, column_aliases, control)?;
    RowSchema::canonical_join_alias_with_control(schema, alias, &columns, control)
}

pub fn alias_join_physical_schema_with_control(
    schema: &RowSchema,
    alias: &str,
    column_aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let columns = join_alias_columns_with_control(schema, alias, column_aliases, control)?;
    RowSchema::remap_join_alias_with_control(schema, alias, &columns, control)
}
