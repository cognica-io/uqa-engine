//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical OLD/NEW row images and RETURNING projection results.
use crate::{
    mutation::{
        errors::dml_storage_error,
        row_images::{MutationRowImage, MutationRowImages},
        rows::context::MutationRowContext,
    },
    query::{relational::RelationalContext, CteScope},
    OwnedPhysicalRow, PhysicalRow, RowSchema,
};
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::{
    ast::ReturningAliases,
    plan::ProjectionPlan,
    routines::RoutineResolution,
    semantics::{
        doc_id_value,
        returning::{expanded_returning_projections, returning_target_schema, ReturningCatalog},
        returning_context_schema, returning_expression_schema, DOC_ID_COLUMN, TABLE_OID_COLUMN,
    },
    SQLError, SQLParam, SQLResult,
};

#[derive(Clone)]
pub struct ReturningExecutionContext<'a, S: Clone + 'static> {
    pub rows: MutationRowContext<'a>,
    pub catalog: &'a dyn ReturningCatalog,
    pub routines: &'a dyn RoutineResolution,
    pub relational: RelationalContext<'a, S>,
}
impl<S: Clone + 'static> Copy for ReturningExecutionContext<'_, S> {}

#[derive(Clone)]
pub struct ReturningProjectionRow<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub images: MutationRowImages<'a>,
    pub aliases: &'a ReturningAliases,
    pub context: Option<&'a OwnedPhysicalRow>,
}

#[derive(Clone, Copy)]
pub struct ReturningValueProjectionRow<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub current: &'a [Value],
    pub old: Option<&'a [Value]>,
    pub new: Option<&'a [Value]>,
    pub aliases: &'a ReturningAliases,
    pub context: Option<&'a OwnedPhysicalRow>,
}

pub struct DmlReturningShape<'a, S: Clone + 'static> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub aliases: &'a ReturningAliases,
    pub returning: &'a [ProjectionPlan],
    pub params: &'a [SQLParam],
    pub ctes: &'a CteScope<S>,
    pub supplemental_schema: Option<&'a RowSchema>,
}

pub fn returning_image_values<S: Clone + 'static>(
    services: ReturningExecutionContext<'_, S>,
    image: Option<&MutationRowImage<'_>>,
    columns: &[String],
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Result<Vec<Value>, SQLError> {
    let Some(image) = image else {
        return Ok(vec![Value::Null; columns.len()]);
    };
    let mut document = image.document.clone();
    crate::query::generated::materialize_virtual_generated_columns(definitions, &mut document)?;
    columns
        .iter()
        .map(|column| {
            if super::rows::is_virtual_document_id_column(column, definitions)
                || definitions.iter().any(|definition| {
                    definition.name == *column
                        && definition.primary_key
                        && definition.ty.is_integer()
                })
            {
                doc_id_value(image.doc_id)
            } else if column == TABLE_OID_COLUMN {
                Ok(Value::Int(crate::catalog::projection::table_relation_oid(
                    &services.rows.catalog,
                    &image.storage_table,
                )?))
            } else {
                Ok(crate::query::document_projection::project_document_column(
                    &document,
                    image.metadata,
                    column,
                    definitions,
                ))
            }
        })
        .collect()
}

pub fn build_returning_row<S: Clone + 'static>(
    services: ReturningExecutionContext<'_, S>,
    input: ReturningProjectionRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let row = returning_projection_context(services, input.clone())?;
    let projections = expanded_returning_projections(
        services.catalog,
        input.table,
        input.target_qualifier,
        input.aliases,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    crate::query::relational::project_row::build_projection_physical_row_with_ctes(
        services.relational,
        &row,
        &projections,
        params,
        &snapshot_scope,
    )
}

pub fn build_returning_value_row<S: Clone + 'static>(
    services: ReturningExecutionContext<'_, S>,
    input: ReturningValueProjectionRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let row = returning_value_context(services, input)?;
    let projections = expanded_returning_projections(
        services.catalog,
        input.table,
        input.target_qualifier,
        input.aliases,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    crate::query::relational::project_row::build_projection_physical_row_with_ctes(
        services.relational,
        &row,
        &projections,
        params,
        &snapshot_scope,
    )
}

pub fn returning_value_context<S: Clone + 'static>(
    services: ReturningExecutionContext<'_, S>,
    input: ReturningValueProjectionRow<'_>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let target = returning_target_schema(services.catalog, input.table)?;
    let width = target.len();
    if input.current.len() != width
        || input.old.is_some_and(|row| row.len() != width)
        || input.new.is_some_and(|row| row.len() != width)
    {
        return Err(SQLError::Internal(
            "rewrite-rule RETURNING row does not match the event relation".into(),
        ));
    }
    let mut columns = target.columns().to_vec();
    let mut types = target.column_types().to_vec();
    let append_doc_id = !columns.iter().any(|column| column == DOC_ID_COLUMN);
    if append_doc_id {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(uqa_sql::ast::ColumnType::BigInteger));
    }
    let image = |row: Option<&[Value]>| {
        let mut values = row.map_or_else(|| vec![Value::Null; width], <[Value]>::to_vec);
        if append_doc_id {
            values.push(Value::Null);
        }
        values
    };
    let mut current = input.current.to_vec();
    if append_doc_id {
        current.push(Value::Null);
    }
    let schema = returning_context_schema(
        &columns,
        &types,
        width,
        input.target_qualifier,
        input.aliases,
    );
    let values = current
        .into_iter()
        .chain(image(input.old))
        .chain(image(input.new))
        .collect();
    let target = OwnedPhysicalRow::new(schema, PhysicalRow::from_values(values));
    Ok(input.context.map_or(target.clone(), |context| {
        OwnedPhysicalRow::new(
            RowSchema::join(&target.schema, &context.schema, std::iter::empty()),
            PhysicalRow::concat(&target.row, &context.row),
        )
    }))
}

pub fn returning_projection_context<S: Clone + 'static>(
    services: ReturningExecutionContext<'_, S>,
    input: ReturningProjectionRow<'_>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let target = returning_row_context(
        services,
        input.table,
        input.target_qualifier,
        input.images,
        input.aliases,
    )?;
    let row = input.context.map_or(target.clone(), |context| {
        OwnedPhysicalRow::new(
            RowSchema::join(&target.schema, &context.schema, std::iter::empty()),
            PhysicalRow::concat(&target.row, &context.row),
        )
    });
    Ok(row)
}

pub fn dml_returning_result<S: Clone + 'static>(
    services: ReturningExecutionContext<'_, S>,
    shape: DmlReturningShape<'_, S>,
    rows: Vec<OwnedPhysicalRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    let projections = expanded_returning_projections(
        services.catalog,
        shape.table,
        shape.target_qualifier,
        shape.aliases,
        shape.returning,
    )?;
    dml_returning_result_with_projections(services, shape, &projections, rows, affected_rows)
}

pub fn dml_returning_result_with_projections<S: Clone + 'static>(
    services: ReturningExecutionContext<'_, S>,
    shape: DmlReturningShape<'_, S>,
    projections: &[ProjectionPlan],
    rows: Vec<OwnedPhysicalRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    let star_schema = returning_target_schema(services.catalog, shape.table)?;
    let expression_schema = returning_expression_schema(
        &star_schema,
        shape.target_qualifier,
        shape.aliases,
        shape.supplemental_schema,
    );
    let output = crate::query::binding::bind_projection_output_schema(
        services.routines,
        projections,
        &expression_schema,
        &star_schema,
        &shape.ctes.scalar_subqueries,
        shape.params,
        shape.ctes,
    )?;
    let preserve_positions =
        output.columns().iter().collect::<BTreeSet<_>>().len() != output.columns().len();
    let mut named_rows = Vec::with_capacity(rows.len());
    let mut positional_rows = preserve_positions.then(|| Vec::with_capacity(rows.len()));
    for row in rows {
        if let Some(positional_rows) = positional_rows.as_mut() {
            positional_rows.push(row.view().iter().map(|(_, value)| value.clone()).collect());
        }
        named_rows.push(row.into_result_row());
    }
    let mut result = SQLResult::from_typed_rows_with_positions(
        output.columns().to_vec(),
        output.column_types().to_vec(),
        named_rows,
        positional_rows,
    );
    result.affected_rows = affected_rows;
    Ok(result)
}

pub fn returning_row_context<S: Clone + 'static>(
    services: ReturningExecutionContext<'_, S>,
    table: &str,
    target_qualifier: &str,
    images: MutationRowImages<'_>,
    aliases: &ReturningAliases,
) -> Result<OwnedPhysicalRow, SQLError> {
    let current = images.new.as_ref().or(images.old.as_ref()).ok_or_else(|| {
        SQLError::Internal(format!(
            "RETURNING for table `{table}` has neither an old nor a new row image"
        ))
    })?;
    let definitions = services
        .rows
        .relations
        .column_definitions(table)
        .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let target = returning_target_schema(services.catalog, table)?;
    let composite_width = target.len();
    let mut columns = target.columns().to_vec();
    let mut types = target.column_types().to_vec();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(uqa_sql::ast::ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(uqa_sql::ast::ColumnType::Oid));
    columns.push(uqa_sql::semantics::XMIN_COLUMN.into());
    types.push(Some(uqa_sql::ast::ColumnType::Xid));
    let schema =
        returning_context_schema(&columns, &types, composite_width, target_qualifier, aliases);
    let current_values = returning_image_values(services, Some(current), &columns, &definitions)?;
    let old_values = returning_image_values(services, images.old.as_ref(), &columns, &definitions)?;
    let new_values = returning_image_values(services, images.new.as_ref(), &columns, &definitions)?;
    let values = current_values
        .into_iter()
        .chain(old_values)
        .chain(new_values)
        .collect();
    Ok(OwnedPhysicalRow::new(
        schema,
        PhysicalRow::from_values(values),
    ))
}
