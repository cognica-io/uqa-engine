//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lateral source execution and correlated query-block remapping.

use super::{
    build_join_operator_with_ctes, build_table_function_row_stream_with_row, eval_scalar,
    prefix_row, projection_columns, query_output_shared, AccessPathPlan, ComputePlan, CteScope,
    Engine, PlanSubqueryArena, QueryBlockPlan, QueryOutput, QueryOutputMode, QueryPlan,
    RelationalPlan, ResultRow, SQLError, SQLParam, ScalarEvalContext, ScalarExpr, ScopedEngineHook,
    SourcePlan, TableFunctionCall, TableFunctionEvalContext, Value,
};

pub(in crate::sql) struct EngineLateralSource<'a> {
    pub(super) engine: &'a Engine,
    pub(super) right: SourcePlan,
    pub(super) on: Option<ScalarExpr>,
    pub(super) params: &'a [SQLParam],
    pub(super) ctes: CteScope,
    pub(super) outer_schema: uqa_execution::RowSchema,
}

impl uqa_execution::LateralSource for EngineLateralSource<'_> {
    fn rows_for(
        &mut self,
        left_row: &ResultRow,
    ) -> uqa_execution::ExecResult<uqa_execution::LateralRows> {
        if let SourcePlan::Function {
            name,
            relation,
            args,
            alias,
            column_aliases,
            column_types,
        } = &self.right
        {
            let hook = ScopedEngineHook::new(self.engine, &self.ctes);
            let context = TableFunctionEvalContext::new(
                self.engine,
                self.params,
                &hook,
                &hook,
                &self.ctes.scalar_subqueries,
            );
            let call = TableFunctionCall::new(
                name,
                relation.as_deref(),
                args,
                alias.as_deref(),
                column_aliases,
                column_types,
            );
            return Ok(build_table_function_row_stream_with_row(
                &context,
                call,
                Some(left_row),
            )?);
        }
        match &self.right {
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let output = execute_lateral_subquery_output_with_outer_schema(
                    self.engine,
                    body,
                    left_row,
                    self.params,
                    &self.ctes,
                    &self.outer_schema,
                )?;
                let source_columns = output.internal_columns.clone();
                let rows = query_output_shared(output, "lateral subquery")?;
                let reader = rows.read_rows()?;
                let alias = alias.clone();
                let aliases = column_aliases.clone();
                Ok(Box::new(reader.map(move |row| {
                    let row = row?.into_result_row();
                    Ok(remap_subquery_row(
                        row,
                        &source_columns,
                        alias.as_deref(),
                        &aliases,
                    ))
                })))
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
                Ok(Box::new(rows.read_rows()?.map(|row| {
                    row.map(uqa_execution::OwnedPhysicalRow::into_result_row)
                })))
            }
        }
    }

    fn matches(&mut self, joined: &ResultRow) -> uqa_execution::ExecResult<bool> {
        let Some(filter) = self.on.as_ref() else {
            return Ok(true);
        };
        let scoped_hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let subquery_arena =
            PlanSubqueryArena::new(&self.ctes.scalar_subqueries, Some(&scoped_hook));
        let context = ScalarEvalContext::new(Some(joined), self.params)
            .with_function_hook(&scoped_hook)
            .with_subquery_runner(&subquery_arena);
        Ok(uqa_sql::expr::truthy(&eval_scalar(filter, &context)?))
    }
}

/// Build the engine-specific correlated source and execute it through the
/// common physical `LateralJoin` operator.
#[allow(clippy::too_many_arguments)]
pub(in crate::sql) fn execute_lateral_subquery_output(
    engine: &Engine,
    plan: &QueryPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<QueryOutput, SQLError> {
    execute_lateral_subquery_output_inner(engine, plan, outer_row, params, ctes, None)
}

pub(in crate::sql) fn execute_lateral_subquery_output_with_outer_schema(
    engine: &Engine,
    plan: &QueryPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &CteScope,
    outer_schema: &uqa_execution::RowSchema,
) -> Result<QueryOutput, SQLError> {
    execute_lateral_subquery_output_inner(engine, plan, outer_row, params, ctes, Some(outer_schema))
}

fn execute_lateral_subquery_output_inner(
    engine: &Engine,
    plan: &QueryPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &CteScope,
    outer_schema: Option<&uqa_execution::RowSchema>,
) -> Result<QueryOutput, SQLError> {
    let mut scoped_ctes = ctes.clone();
    crate::sql::select::materialize_plan_ctes(engine, &plan.ctes, params, &mut scoped_ctes)?;
    execute_lateral_relational_root_output(
        engine,
        &plan.root,
        outer_row,
        params,
        &mut scoped_ctes,
        outer_schema,
    )
}

pub(in crate::sql) fn execute_lateral_relational_root_output(
    engine: &Engine,
    root: &RelationalPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &mut CteScope,
    outer_schema: Option<&uqa_execution::RowSchema>,
) -> Result<QueryOutput, SQLError> {
    match root {
        RelationalPlan::QueryBlock(block) => {
            execute_lateral_query_block_output(engine, block, outer_row, params, ctes, outer_schema)
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
                outer_schema,
            )?;
            let columns = lhs.columns.clone();
            let lhs = query_output_shared(lhs, "lateral set left")?;
            let rhs = execute_lateral_subquery_output_inner(
                engine,
                right,
                outer_row,
                params,
                &scoped_ctes,
                outer_schema,
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
            let context = crate::sql::scalar::PhysicalEvalContext::new(Some(outer_row), params)
                .with_function_hook(&hook)
                .with_subquery_runner(&hook);
            let rows = rows
                .iter()
                .map(|values| {
                    let mut row = ResultRow::new();
                    for (index, expression) in values.iter().enumerate() {
                        row.insert(
                            columns[index].clone(),
                            crate::sql::scalar::eval_physical_scalar(
                                expression, subqueries, &context,
                            )?,
                        );
                    }
                    Ok(row)
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
                Box::new(uqa_execution::TableScan::from_rows(columns.clone(), rows));
            crate::sql::select::collect_query_operator(
                engine,
                columns,
                operator,
                QueryOutputMode::SharedSpill,
            )
        }
    }
}

pub(in crate::sql) fn execute_lateral_query_block_output(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    scoped_ctes: &mut CteScope,
    outer_schema: Option<&uqa_execution::RowSchema>,
) -> Result<QueryOutput, SQLError> {
    let mut scoped_ctes = scoped_ctes.enter_scalar_subqueries(&stmt.subqueries);
    let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
        if let Some(from) = stmt.from.as_ref() {
            let child =
                build_join_operator_with_ctes(engine, from, params, &mut scoped_ctes, None, None)?;
            match outer_schema {
                Some(outer_schema) => Box::new(uqa_execution::ScopeOverlay::new_with_outer_schema(
                    child,
                    outer_row.clone(),
                    outer_schema,
                )),
                None => Box::new(uqa_execution::ScopeOverlay::new(child, outer_row.clone())),
            }
        } else {
            let child: Box<dyn uqa_execution::PhysicalOperator + '_> = Box::new(
                uqa_execution::TableScan::from_rows(Vec::new(), vec![ResultRow::new()]),
            );
            match outer_schema {
                Some(outer_schema) => Box::new(uqa_execution::ScopeOverlay::new_with_outer_schema(
                    child,
                    outer_row.clone(),
                    outer_schema,
                )),
                None => Box::new(uqa_execution::ScopeOverlay::new(child, outer_row.clone())),
            }
        };
    let columns = projection_columns(&stmt.projections);
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

pub(in crate::sql) fn remap_subquery_row(
    mut row: ResultRow,
    source_columns: &[String],
    alias: Option<&str>,
    column_aliases: &[String],
) -> ResultRow {
    let mut output = ResultRow::new();
    for (index, source) in source_columns.iter().enumerate() {
        let target = column_aliases
            .get(index)
            .cloned()
            .unwrap_or_else(|| source.clone());
        let value = row.remove(source).unwrap_or(Value::Null);
        output.insert(target, value);
    }
    match alias {
        Some(alias) => prefix_row(alias, &output),
        None => output,
    }
}
