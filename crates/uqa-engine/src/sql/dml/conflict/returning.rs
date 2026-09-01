//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_projection_output_schema, build_projection_physical_row_with_ctes, dml_storage_error,
    doc_id_value, returning_row_context, BTreeSet, ColumnIdentity, CteScope, DocId, Document,
    Engine, MutationRowImage, MutationRowImages, OwnedPhysicalRow, PhysicalRow, ProjectionPlan,
    ReturningAliases, RowSchema, SQLError, SQLParam, SQLResult, Statement, Value, DOC_ID_COLUMN,
    TABLE_OID_COLUMN,
};

pub(super) fn returning_image_values(
    engine: &Engine,
    image: Option<&MutationRowImage<'_>>,
    columns: &[String],
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Result<Vec<Value>, SQLError> {
    let Some(image) = image else {
        return Ok(vec![Value::Null; columns.len()]);
    };
    let mut document = image.document.clone();
    crate::engine_generated::materialize_virtual_generated_columns(definitions, &mut document)?;
    columns
        .iter()
        .map(|column| {
            if super::super::is_virtual_document_id_column(column, definitions)
                || definitions.iter().any(|definition| {
                    definition.name == *column
                        && definition.primary_key
                        && definition.ty.is_integer()
                })
            {
                doc_id_value(image.doc_id)
            } else if column == TABLE_OID_COLUMN {
                Ok(Value::Int(crate::sql::catalog::table_relation_oid(
                    engine,
                    &image.storage_table,
                )?))
            } else {
                Ok(document.get(column).cloned().unwrap_or(Value::Null))
            }
        })
        .collect()
}

pub(super) fn returning_context_schema(
    columns: &[String],
    types: &[Option<uqa_sql::ast::ColumnType>],
    target_qualifier: &str,
    aliases: &ReturningAliases,
) -> RowSchema {
    let target =
        RowSchema::with_qualified_types(target_qualifier, columns.to_vec(), types.to_vec());
    let hidden_types = types
        .iter()
        .cloned()
        .chain(types.iter().cloned())
        .collect::<Vec<_>>();
    let schema = RowSchema::append_hidden_typed(&target, &hidden_types);
    let width = columns.len();
    let identity_aliases = columns
        .iter()
        .enumerate()
        .flat_map(|(position, column)| {
            [
                (
                    ColumnIdentity::qualified(&aliases.old, column),
                    width + position,
                    types[position].clone(),
                ),
                (
                    ColumnIdentity::qualified(&aliases.new, column),
                    width * 2 + position,
                    types[position].clone(),
                ),
            ]
        })
        .collect::<Vec<_>>();
    RowSchema::with_physical_identity_aliases(&schema, &identity_aliases)
}

#[derive(Clone)]
pub(in crate::sql) struct ReturningProjectionRow<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub images: MutationRowImages<'a>,
    pub aliases: &'a ReturningAliases,
    pub context: Option<&'a OwnedPhysicalRow>,
}

#[derive(Clone, Copy)]
pub(in crate::sql) struct ReturningValueProjectionRow<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub current: &'a [Value],
    pub old: Option<&'a [Value]>,
    pub new: Option<&'a [Value]>,
    pub aliases: &'a ReturningAliases,
    pub context: Option<&'a OwnedPhysicalRow>,
}

pub(in crate::sql) fn build_returning_row(
    engine: &Engine,
    input: ReturningProjectionRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<OwnedPhysicalRow, SQLError> {
    let row = returning_projection_context(engine, input.clone())?;
    let projections = expanded_returning_projections(
        engine,
        input.table,
        input.target_qualifier,
        input.aliases,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, &snapshot_scope)
}

/// Project a RETURNING row supplied positionally by a rewrite-rule action.
/// Rule target lists describe the event relation's row type but are not
/// storage documents, so integer primary-key values must remain ordinary
/// values rather than being reconstructed from an internal document id.
pub(in crate::sql) fn build_returning_value_row(
    engine: &Engine,
    input: ReturningValueProjectionRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<OwnedPhysicalRow, SQLError> {
    let row = returning_value_context(engine, input)?;
    let projections = expanded_returning_projections(
        engine,
        input.table,
        input.target_qualifier,
        input.aliases,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, &snapshot_scope)
}

pub(in crate::sql) fn returning_value_context(
    engine: &Engine,
    input: ReturningValueProjectionRow<'_>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let target = returning_target_schema(engine, input.table)?;
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
    let schema = returning_context_schema(&columns, &types, input.target_qualifier, input.aliases);
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

pub(in crate::sql) fn returning_projection_context(
    engine: &Engine,
    input: ReturningProjectionRow<'_>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let target = returning_row_context(
        engine,
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

pub(in crate::sql) struct DmlReturningShape<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub aliases: &'a ReturningAliases,
    pub returning: &'a [ProjectionPlan],
    pub params: &'a [SQLParam],
    pub ctes: &'a CteScope,
    pub supplemental_schema: Option<&'a RowSchema>,
}

/// Derive a DML statement's declared RETURNING row type without executing the
/// statement. Rewrite-rule registration uses this to enforce `PostgreSQL`'s
/// positional event-row contract before the rule reaches durable storage.
pub(in crate::sql) fn dml_statement_returning_schema(
    engine: &Engine,
    statement: Statement,
) -> Result<Option<RowSchema>, SQLError> {
    let plan = crate::sql::lower_statement(engine, statement);
    let uqa_planner::UnifiedPlan::Command(command) = plan else {
        return Ok(None);
    };
    dml_command_returning_schema(engine, &command, &[])
}

pub(in crate::sql) fn dml_command_returning_schema(
    engine: &Engine,
    command: &uqa_planner::CommandPlan,
    params: &[SQLParam],
) -> Result<Option<RowSchema>, SQLError> {
    match command {
        uqa_planner::CommandPlan::Insert(plan) => analyze_dml_returning_plan(
            engine,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            &plan.ctes,
            None,
            &plan.subqueries,
            params,
        ),
        uqa_planner::CommandPlan::Update(plan) => analyze_dml_returning_plan(
            engine,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            &plan.ctes,
            plan.source.as_deref(),
            &plan.subqueries,
            params,
        ),
        uqa_planner::CommandPlan::Delete(plan) => analyze_dml_returning_plan(
            engine,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            &plan.ctes,
            plan.source.as_deref(),
            &plan.subqueries,
            params,
        ),
        _ => Ok(None),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
fn analyze_dml_returning_plan(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    returning: &[ProjectionPlan],
    cte_plans: &[uqa_planner::CtePlan],
    source: Option<&uqa_planner::SourcePlan>,
    subqueries: &[uqa_planner::QueryPlan],
    params: &[SQLParam],
) -> Result<Option<RowSchema>, SQLError> {
    if returning.is_empty() {
        return Ok(None);
    }
    let mut ctes = CteScope::new_for_current_routine(engine);
    for plan in cte_plans {
        ctes.insert_deferred(plan.clone());
    }
    ctes.scalar_subqueries = subqueries.to_vec();
    let supplemental = source
        .map(|source| {
            crate::sql::select::analyze_source_plan_schema(engine, source, params, &ctes, None)
        })
        .transpose()?;
    let star_schema = returning_target_schema(engine, table)?;
    let expression_schema = returning_expression_schema(
        &star_schema,
        target_qualifier,
        aliases,
        supplemental.as_ref(),
    );
    let projections =
        expanded_returning_projections(engine, table, target_qualifier, aliases, returning)?;
    crate::sql::select::analyze_projection_output_schema(
        engine,
        &projections,
        &expression_schema,
        &star_schema,
        subqueries,
        params,
        &ctes,
    )
    .map(Some)
}

pub(in crate::sql) fn dml_returning_result(
    engine: &Engine,
    shape: DmlReturningShape<'_>,
    rows: Vec<OwnedPhysicalRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    let projections = expanded_returning_projections(
        engine,
        shape.table,
        shape.target_qualifier,
        shape.aliases,
        shape.returning,
    )?;
    dml_returning_result_with_projections(engine, shape, &projections, rows, affected_rows)
}

pub(in crate::sql) fn dml_returning_result_with_projections(
    engine: &Engine,
    shape: DmlReturningShape<'_>,
    projections: &[ProjectionPlan],
    rows: Vec<OwnedPhysicalRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    let star_schema = returning_target_schema(engine, shape.table)?;
    let expression_schema = returning_expression_schema(
        &star_schema,
        shape.target_qualifier,
        shape.aliases,
        shape.supplemental_schema,
    );
    let output = bind_projection_output_schema(
        engine,
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

pub(super) fn returning_target_schema(engine: &Engine, table: &str) -> Result<RowSchema, SQLError> {
    let definitions = engine
        .try_describe_table_row_type(table)
        .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?;
    let Some(definitions) = definitions else {
        return engine
            .view_schema(table)?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()));
    };
    if definitions.is_empty() {
        let columns = engine
            .try_table_columns(table)
            .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?;
        let width = columns.len();
        return Ok(RowSchema::with_types(columns, vec![None; width]));
    }
    let columns = definitions
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    let types = definitions
        .into_iter()
        .map(|definition| Some(definition.ty))
        .collect();
    Ok(RowSchema::with_types(columns, types))
}

fn returning_expression_schema(
    target: &RowSchema,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    supplemental: Option<&RowSchema>,
) -> RowSchema {
    let mut columns = target.columns().to_vec();
    let mut types = target.column_types().to_vec();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(uqa_sql::ast::ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(uqa_sql::ast::ColumnType::Oid));
    columns.push(crate::sql::XMIN_COLUMN.into());
    types.push(Some(uqa_sql::ast::ColumnType::Xid));
    let target = returning_context_schema(&columns, &types, target_qualifier, aliases);
    supplemental.map_or(target.clone(), |source| {
        RowSchema::join(&target, source, std::iter::empty())
    })
}

pub(in crate::sql) fn expanded_returning_projections(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    returning: &[ProjectionPlan],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let columns = returning_target_schema(engine, table)?.columns().to_vec();
    let mut projections = Vec::with_capacity(returning.len().max(columns.len()));
    for projection in returning {
        match &projection.expr {
            uqa_execution::ScalarExpr::Star => {
                projections.extend(columns.iter().map(|column| ProjectionPlan {
                    expr: uqa_execution::ScalarExpr::Column(column.clone()),
                    alias: Some(column.clone()),
                }));
            }
            uqa_execution::ScalarExpr::QualifiedStar(qualifier)
                if qualifier == target_qualifier
                    || qualifier == &aliases.old
                    || qualifier == &aliases.new =>
            {
                projections.extend(columns.iter().map(|column| ProjectionPlan {
                    expr: uqa_execution::ScalarExpr::QualifiedColumn {
                        qualifier: qualifier.clone(),
                        column: column.clone(),
                    },
                    alias: Some(column.clone()),
                }));
            }
            _ => projections.push(projection.clone()),
        }
    }
    Ok(projections)
}

pub(in crate::sql) fn document_supplied_id(
    document: &Document,
    id_column: &str,
    auto_increment: bool,
) -> Result<Option<DocId>, SQLError> {
    match document.get(id_column) {
        Some(Value::Int(value)) if *value >= 0 => Ok(Some(*value as DocId)),
        Some(Value::Null) | None => Ok(None),
        Some(other) if auto_increment => Err(SQLError::TypeMismatch(format!(
            "auto-increment id must be an integer, got {other:?}"
        ))),
        Some(_) => Ok(None),
    }
}

// -------------------------------------------------------------------------

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------
