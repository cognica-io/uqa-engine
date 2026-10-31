//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation clause namespaces admit only addressable target/source metadata; row-image aliases belong to RETURNING.

use super::{DOC_ID_COLUMN, TABLE_OID_COLUMN};
use crate::{ColumnType, RowSchema, SQLError};
use uqa_core::memory::ProductionControl;

pub fn mutation_clause_schema_with_control(
    target: &RowSchema,
    qualifier: &str,
    supplemental: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let schema = mutation_target_schema(target, qualifier, control)?;
    match supplemental {
        Some(source) => RowSchema::join_with_control(&schema, source, std::iter::empty(), control),
        None => Ok(schema),
    }
}

pub fn returning_expression_schema_with_control(
    target: &RowSchema,
    qualifier: &str,
    aliases: &crate::ast::ReturningAliases,
    supplemental: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let schema = mutation_target_schema(target, qualifier, control)?;
    let schema = RowSchema::with_row_image_aliases_with_control(
        &schema,
        target.len(),
        &aliases.old,
        &aliases.new,
        control,
    )?;
    match supplemental {
        Some(source) => RowSchema::join_with_control(&schema, source, std::iter::empty(), control),
        None => Ok(schema),
    }
}

fn mutation_target_schema(
    target: &RowSchema,
    qualifier: &str,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let columns = target.columns().iter().enumerate().map(|(position, name)| {
        (
            name.as_str(),
            target.column_types()[position].as_ref(),
            target.column_collation(position),
        )
    });
    let doc_id = (!target.columns().iter().any(|name| name == DOC_ID_COLUMN)).then_some((
        DOC_ID_COLUMN,
        Some(&ColumnType::BigInteger),
        None,
    ));
    let system = [
        (TABLE_OID_COLUMN, Some(&ColumnType::Oid), None),
        (crate::semantics::XMIN_COLUMN, Some(&ColumnType::Xid), None),
    ];
    let schema = RowSchema::with_column_metadata_with_control(
        Some(qualifier),
        columns.chain(doc_id).chain(system),
        control,
    )?;
    let schema = RowSchema::with_wildcard_hidden_positions_with_control(
        &schema,
        target.len()..schema.len(),
        control,
    )?;
    Ok(schema)
}
