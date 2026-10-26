//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-table pushdown and execution.

use super::{
    execute_query_block_operator_output, expand_from_star_columns, projection_columns, CteScope,
    QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError, SQLParam, SingleRelation,
    SourceContext,
};

mod predicates;
pub use predicates::*;

pub fn run_single_foreign_select_output<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    relation: SingleRelation<'_>,
    block: &'a QueryBlockPlan,
    stmt: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
    output_mode: QueryOutputMode<'a>,
) -> Result<QueryOutput, SQLError> {
    let SingleRelation {
        reference_name: _,
        relation_name: table,
        qualifier,
    } = relation;
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    let foreign_table = catalog
        .foreign_table_resolved(&resolution, table)?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let definitions = &catalog.snapshot().definitions;
    foreign_table
        .bound_server(&definitions.foreign_servers)?
        .bound_wrapper(&definitions.foreign_wrappers)?
        .require_handler()?;
    let scanned = context
        .scan_foreign_source(table, stmt.r#where.as_ref(), params)
        .map_err(SQLError::Unsupported)?;
    let source_schema =
        crate::RowSchema::with_column_definitions(Some(qualifier), &foreign_table.columns);
    let source: Box<dyn crate::PhysicalOperator + '_> =
        Box::new(crate::RowIteratorScan::with_row_schema(
            source_schema,
            Box::new(scanned.map(|row| {
                row.map_err(SQLError::Unsupported)
                    .map_err(crate::ExecError::from)
            })),
        ));
    let bound_columns = match stmt.from.as_ref() {
        Some(uqa_sql::plan::SourcePlan::Table { bound_columns, .. }) => bound_columns.as_deref(),
        _ => None,
    };
    let source = crate::query::source_projection::bound_source_operator(source, bound_columns)?;
    let source = context
        .scans
        .runtime
        .diagnostics
        .observe(source, "Foreign Scan")?;
    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        source.row_schema(),
    )?;
    execute_query_block_operator_output(
        context.relational,
        source,
        stmt.r#where.clone(),
        stmt,
        block,
        params,
        ctes,
        columns,
        output_mode,
    )
}
