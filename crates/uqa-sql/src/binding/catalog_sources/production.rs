//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Operator joins publish each independently resolved table namespace through admitted schema producers.

use super::{CatalogReadView, OperatorJoinRelations, RelationNameResolution, RowSchema, SQLError};
use uqa_core::memory::ProductionControl;

pub(in crate::binding) fn operator_join_relation_schemas_with_control(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relations: Option<&OperatorJoinRelations>,
    control: &ProductionControl<'_>,
) -> Result<(RowSchema, RowSchema), SQLError> {
    control.check()?;
    let relations = relations.ok_or_else(|| {
        SQLError::TypeMismatch("operator join requires left and right table identifiers".into())
    })?;
    Ok((
        relation_schema(catalog, resolution, &relations.left, "left", control)?,
        relation_schema(catalog, resolution, &relations.right, "right", control)?,
    ))
}

fn relation_schema(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relation: &str,
    side: &str,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let resolved = catalog
        .table_name_resolved_with_control(resolution, relation, control)?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_owned()))?;
    let identity = crate::RelationIdentity::try_borrowed_reference(&resolved).map_err(|error| {
        SQLError::Internal(format!(
            "decode operator join {side} relation `{}` schema: {error}",
            resolved.as_str()
        ))
    })?;
    let name = control.format(format_args!("{}", identity.name))?;
    let table = catalog
        .table_resolved_with_control(resolution, &resolved, control)?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_owned()))?;
    let schema = RowSchema::with_column_metadata_with_control(
        Some(&name),
        table
            .columns
            .iter()
            .map(|column| (column.name.as_str(), Some(&column.ty), column.collation_oid)),
        control,
    )?;
    super::analysis::with_table_pseudo_columns_with_control(&schema, &name, control)
}
