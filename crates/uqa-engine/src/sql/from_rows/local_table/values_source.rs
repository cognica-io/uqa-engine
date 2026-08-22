//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for VALUES sources.

use super::{
    build_values_physical_rows, qualify_source_operator_with_columns, ColumnPrune, CteScope,
    Engine, SQLError, SQLParam, ScopedEngineHook, SourceEvalContext, SourcePlan,
};
use uqa_execution::PhysicalOperator;

/// Build the physical operator for a VALUES source.
pub(super) fn build_values_source_operator<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &CteScope,
    prune: Option<&ColumnPrune>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
        } => {
            let column_types = crate::sql::select::values_types_in_scope(
                engine,
                rows,
                &ctes.scalar_subqueries,
                None,
                params,
                ctes,
            )?;
            let source_columns = if column_aliases.is_empty() {
                (0..rows.first().map_or(0, Vec::len))
                    .map(|index| format!("column{}", index + 1))
                    .collect::<Vec<_>>()
            } else {
                column_aliases.clone()
            };
            let hook = ScopedEngineHook::new(engine, ctes);
            let context =
                SourceEvalContext::new(engine, params, &hook, &hook, &ctes.scalar_subqueries);
            let rows = build_values_physical_rows(&context, rows, &column_types)?;
            let schema = uqa_execution::RowSchema::with_types(source_columns.clone(), column_types);
            let operator: Box<dyn uqa_execution::PhysicalOperator + 'a> =
                Box::new(uqa_execution::TableScan::from_physical_rows(schema, rows));
            Ok(qualify_source_operator_with_columns(
                operator,
                &source_columns,
                alias.as_deref().unwrap_or_default(),
                prune,
                &[],
                ctes.lock_identities.emit,
            ))
        }
        _ => unreachable!("VALUES source builder called for a different source kind"),
    }
}
