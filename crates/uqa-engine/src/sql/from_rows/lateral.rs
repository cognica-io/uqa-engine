//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lateral source execution and correlated query-block remapping.

use super::{
    build_join_operator_with_ctes, build_table_function_row_stream_with_row, eval_scalar,
    multi_unnest_internal_columns, projection_columns, query_output_shared, AccessPathPlan,
    ComputePlan, CteScope, Engine, PlanSubqueryArena, QueryBlockPlan, QueryOutput, QueryOutputMode,
    QueryPlan, RelationalPlan, SQLError, SQLParam, ScalarEvalContext, ScalarExpr, ScopedEngineHook,
    SourceEvalContext, SourcePlan, TableFunctionCall,
};
use crate::sql::select::expand_from_star_columns;

pub(in crate::sql) struct EngineLateralSource<'a> {
    pub(super) engine: &'a Engine,
    pub(super) right: SourcePlan,
    pub(super) on: Option<ScalarExpr>,
    pub(super) params: &'a [SQLParam],
    pub(super) ctes: CteScope,
    pub(super) right_schema: uqa_execution::RowSchema,
}

impl uqa_execution::LateralSource for EngineLateralSource<'_> {
    fn rows_for(
        &mut self,
        left_row: &uqa_execution::OwnedPhysicalRow,
    ) -> uqa_execution::ExecResult<uqa_execution::LateralRows> {
        if let SourcePlan::Function {
            name,
            output_name,
            relation,
            args,
            alias,
            column_aliases,
            column_types,
        } = &self.right
        {
            let hook = ScopedEngineHook::new(self.engine, &self.ctes);
            let context = SourceEvalContext::new(
                self.engine,
                self.params,
                &hook,
                &hook,
                &self.ctes.scalar_subqueries,
            );
            let call = TableFunctionCall::new(
                name,
                output_name,
                relation.as_deref(),
                args,
                alias.as_deref(),
                column_aliases,
                column_types,
            );
            let rows = build_table_function_row_stream_with_row(&context, call, Some(left_row))?;
            let schema = self.right_schema.clone();
            let input_schema =
                if crate::sql::builtin_function_dispatch_name(name) == "unnest" && args.len() > 1 {
                    uqa_execution::RowSchema::with_identities(
                        multi_unnest_internal_columns(args.len()),
                        schema.identities().to_vec(),
                        schema.column_types().to_vec(),
                    )
                } else {
                    schema.clone()
                };
            return Ok(Box::new(rows.map(move |row| {
                row.map(|row| {
                    let physical = uqa_execution::PhysicalRow::from_result_row(&input_schema, row);
                    uqa_execution::OwnedPhysicalRow::new(schema.clone(), physical)
                })
            })));
        }
        match &self.right {
            SourcePlan::Subquery { body, .. } => {
                let output = execute_lateral_subquery_output(
                    self.engine,
                    body,
                    left_row,
                    self.params,
                    &self.ctes,
                )?;
                let rows = query_output_shared(output, "lateral subquery")?;
                let reader = rows.read_rows()?;
                let schema = self.right_schema.clone();
                Ok(Box::new(
                    reader.map(move |row| row?.relabel(schema.clone())),
                ))
            }
            SourcePlan::Function { .. } => Err(uqa_execution::ExecError::SQL(SQLError::Internal(
                "function source reached the relational-source fallback".into(),
            ))),
            source => {
                let operator = build_join_operator_with_ctes(
                    self.engine,
                    source,
                    self.params,
                    &mut self.ctes,
                    None,
                    None,
                )?;
                let columns = operator.schema().to_vec();
                let output = crate::sql::select::collect_query_operator(
                    self.engine,
                    columns,
                    operator,
                    QueryOutputMode::SharedSpill,
                )?;
                let rows = query_output_shared(output, "lateral source")?;
                let schema = self.right_schema.clone();
                Ok(Box::new(
                    rows.read_rows()?
                        .map(move |row| row?.relabel(schema.clone())),
                ))
            }
        }
    }

    fn matches(
        &mut self,
        joined: &uqa_execution::OwnedPhysicalRow,
    ) -> uqa_execution::ExecResult<bool> {
        let Some(filter) = self.on.as_ref() else {
            return Ok(true);
        };
        let scoped_hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let subquery_arena =
            PlanSubqueryArena::new(&self.ctes.scalar_subqueries, Some(&scoped_hook));
        let context = ScalarEvalContext::from_row_lookup(joined, self.params)
            .with_function_hook(&scoped_hook)
            .with_subquery_runner(&subquery_arena)
            .with_physical_outer_row(&joined.schema, &joined.row);
        Ok(uqa_sql::expr::truthy(&eval_scalar(filter, &context)?))
    }
}

/// Build the engine-specific correlated source and execute it through the
/// common physical `LateralJoin` operator.
#[allow(clippy::too_many_arguments)]
pub(in crate::sql) fn execute_lateral_subquery_output(
    engine: &Engine,
    plan: &QueryPlan,
    outer_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<QueryOutput, SQLError> {
    execute_lateral_subquery_output_inner(engine, plan, outer_row, params, ctes)
}

fn execute_lateral_subquery_output_inner(
    engine: &Engine,
    plan: &QueryPlan,
    outer_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<QueryOutput, SQLError> {
    let mut scoped_ctes = ctes.clone();
    crate::sql::select::materialize_plan_ctes(engine, &plan.ctes, params, &mut scoped_ctes)?;
    execute_lateral_relational_root_output(engine, &plan.root, outer_row, params, &mut scoped_ctes)
}

fn execute_lateral_relational_root_output(
    engine: &Engine,
    root: &RelationalPlan,
    outer_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<QueryOutput, SQLError> {
    match root {
        RelationalPlan::QueryBlock(block) => {
            execute_lateral_query_block_output(engine, block, outer_row, params, ctes)
        }
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
        } => {
            let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
            let lhs = execute_lateral_subquery_output_inner(
                engine,
                left,
                outer_row,
                params,
                &scoped_ctes,
            )?;
            let columns = lhs.columns.clone();
            let lhs = query_output_shared(lhs, "lateral set left")?;
            let rhs = execute_lateral_subquery_output_inner(
                engine,
                right,
                outer_row,
                params,
                &scoped_ctes,
            )?;
            let rhs = query_output_shared(rhs, "lateral set right")?;
            let order_plan =
                (!order_by.is_empty() || limit.is_some() || offset.is_some()).then(|| {
                    QueryBlockPlan {
                        projections: Vec::new(),
                        from: None,
                        r#where: None,
                        compute: ComputePlan::Project,
                        group_by: Vec::new(),
                        grouping_sets: Vec::new(),
                        having: None,
                        order_by: order_by.clone(),
                        limit: limit.as_deref().cloned(),
                        offset: offset.as_deref().cloned(),
                        distinct: false,
                        distinct_on: Vec::new(),
                        subqueries: subqueries.clone(),
                        access: AccessPathPlan::Row,
                        locking: Vec::new(),
                    }
                });
            let execution = crate::sql::select::SetSpillExecution::new(
                *kind,
                *all,
                columns,
                lhs,
                rhs,
                order_plan.as_ref(),
                QueryOutputMode::SharedSpill,
            );
            crate::sql::select::combine_set_spills_with_order_output(
                engine,
                execution,
                params,
                &scoped_ctes,
            )
        }
        RelationalPlan::Values { rows, subqueries } => {
            let columns: Vec<String> = rows
                .first()
                .map(|row| {
                    (0..row.len())
                        .map(|index| format!("column{}", index + 1))
                        .collect()
                })
                .unwrap_or_default();
            let hook = ScopedEngineHook::new(engine, ctes);
            let context =
                crate::sql::scalar::PhysicalEvalContext::from_row_lookup(outer_row, params)
                    .with_function_hook(&hook)
                    .with_subquery_runner(&hook)
                    .with_physical_outer_row(&outer_row.schema, &outer_row.row);
            let rows = rows
                .iter()
                .map(|values| {
                    values
                        .iter()
                        .map(|expression| {
                            crate::sql::scalar::eval_physical_scalar(
                                expression, subqueries, &context,
                            )
                        })
                        .collect::<Result<Vec<_>, SQLError>>()
                        .map(uqa_execution::PhysicalRow::from_values)
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
                Box::new(uqa_execution::TableScan::from_physical_rows(
                    uqa_execution::RowSchema::new(columns.clone()),
                    rows,
                ));
            crate::sql::select::collect_query_operator(
                engine,
                columns,
                operator,
                QueryOutputMode::SharedSpill,
            )
        }
    }
}

fn execute_lateral_query_block_output(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    outer_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
    scoped_ctes: &mut CteScope,
) -> Result<QueryOutput, SQLError> {
    let mut scoped_ctes = scoped_ctes.enter_scalar_subqueries(&stmt.subqueries);
    if let Some(from) = stmt.from.as_ref() {
        crate::sql::select::validate_source_set_contexts_before_build(
            engine,
            from,
            params,
            &scoped_ctes,
            Some(&outer_row.schema),
        )?;
    }
    let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
        if let Some(from) = stmt.from.as_ref() {
            let child =
                build_join_operator_with_ctes(engine, from, params, &mut scoped_ctes, None, None)?;
            Box::new(uqa_execution::ScopeOverlay::new(child, outer_row.clone()))
        } else {
            let child: Box<dyn uqa_execution::PhysicalOperator + '_> =
                Box::new(uqa_execution::TableScan::from_physical_rows(
                    uqa_execution::RowSchema::default(),
                    vec![uqa_execution::PhysicalRow::default()],
                ));
            Box::new(uqa_execution::ScopeOverlay::new(child, outer_row.clone()))
        };
    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        operator.row_schema(),
    )?;
    crate::sql::select::execute_query_block_operator_output(
        engine,
        operator,
        stmt.r#where.clone(),
        stmt,
        stmt,
        params,
        &scoped_ctes,
        columns,
        QueryOutputMode::SharedSpill,
    )
}
