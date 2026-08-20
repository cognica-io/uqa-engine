//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical set-returning projection operator.

use uqa_execution::RowProjectionValue;

use super::{
    eval_call_arguments, Batch, ColumnType, CteScope, Engine, ExecResult, OwnedPhysicalRow,
    PhysicalOperator, PhysicalRow, PlanSubqueryArena, RowSchema, SQLError, SQLParam,
    ScalarEvalContext, ScalarExpr, ScopedEngineHook, SetExpansion, SetFunctionCall,
    SetFunctionState, SetProjectionPlan, SharedExpressionEvaluator, Value,
};

pub(in crate::sql) struct SetProjection<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
    plan: SetProjectionPlan,
    schema: RowSchema,
    evaluation_schema: RowSchema,
    pass_through: bool,
    output_batch_size: usize,
    input: std::vec::IntoIter<OwnedPhysicalRow>,
    expansion: Option<SetExpansion>,
    exhausted: bool,
}

fn set_call_output_type(
    engine: &Engine,
    call: &SetFunctionCall,
    input_schema: &RowSchema,
    params: &[SQLParam],
) -> Option<ColumnType> {
    let expression = ScalarExpr::Func {
        name: call.name.clone(),
        binding: call.binding.clone(),
        args: call.args.clone(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    uqa_execution::scalar_type_with_resolver(&expression, input_schema, params, engine)
        .ok()
        .flatten()
}

impl<'a> SetProjection<'a> {
    pub(super) fn from_plan(
        child: Box<dyn PhysicalOperator + 'a>,
        engine: &'a Engine,
        params: &'a [SQLParam],
        ctes: &CteScope,
        evaluator: SharedExpressionEvaluator<'a>,
        plan: SetProjectionPlan,
        pass_through: bool,
    ) -> Self {
        let projections = &plan.projections;
        let call_types = plan
            .calls
            .iter()
            .map(|call| set_call_output_type(engine, call, child.row_schema(), params))
            .collect::<Vec<_>>();
        let appended = projections
            .iter()
            .zip(&call_types)
            .filter(|((_, expression), _)| !matches!(expression, ScalarExpr::Star))
            .map(|((name, _), ty)| (name.clone(), ty.clone()))
            .collect::<Vec<_>>();
        let schema = if pass_through {
            RowSchema::append_typed(child.row_schema(), &appended)
        } else {
            let mut columns = Vec::new();
            let mut types = Vec::new();
            for ((name, expression), ty) in projections.iter().zip(&call_types) {
                if matches!(expression, ScalarExpr::Star) {
                    for (position, column) in child.schema().iter().enumerate() {
                        columns.push(column.clone());
                        types.push(child.row_schema().column_type(position).cloned());
                    }
                } else {
                    columns.push(name.clone());
                    types.push(ty.clone());
                }
            }
            RowSchema::with_types(columns, types)
        };
        let evaluation_columns = plan
            .calls
            .iter()
            .zip(call_types)
            .map(|(call, ty)| (call.placeholder.clone(), ty))
            .collect::<Vec<_>>();
        let evaluation_schema = RowSchema::append_typed(child.row_schema(), &evaluation_columns);
        let output_batch_size = plan.output_batch_size.max(1);
        Self {
            child,
            engine,
            params,
            ctes: ctes.clone(),
            evaluator,
            plan,
            schema,
            evaluation_schema,
            pass_through,
            output_batch_size,
            input: Vec::new().into_iter(),
            expansion: None,
            exhausted: false,
        }
    }

    fn next_input(&mut self) -> ExecResult<Option<OwnedPhysicalRow>> {
        loop {
            if let Some(row) = self.input.next() {
                return Ok(Some(row));
            }
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            self.input = batch.into_owned_rows().into_iter();
        }
    }

    fn call_state(
        &self,
        call: &SetFunctionCall,
        row: &OwnedPhysicalRow,
    ) -> ExecResult<SetFunctionState> {
        let identity = call.name.to_ascii_lowercase();
        if crate::sql::builtin_function_dispatch_name(&identity) == "unnest" && call.args.len() != 1
        {
            return Err(SQLError::UnknownFunction(
                "unnest with multiple arrays is only valid in FROM".into(),
            )
            .into());
        }
        if !self.engine.has_registered_table_function(&identity)
            && self.engine.lookup_sql_functions(&call.name).is_some()
        {
            let hook = ScopedEngineHook::new(self.engine, &self.ctes);
            let subqueries = PlanSubqueryArena::new(&self.ctes.scalar_subqueries, Some(&hook));
            let context = ScalarEvalContext::from_row_lookup(row, self.params)
                .with_function_hook(&hook)
                .with_subquery_runner(&subqueries)
                .with_physical_outer_row(&row.schema, &row.row);
            let arguments = eval_call_arguments(&call.args, &context)?;
            let returns_set = crate::sql::plpgsql_exec::resolved_user_function_returns_set(
                self.engine,
                &call.name,
                &arguments,
            )
            .ok_or_else(|| {
                uqa_execution::ExecError::Other(format!(
                    "user function `{}` disappeared during projection",
                    call.name
                ))
            })??;
            if !returns_set {
                let value = crate::sql::plpgsql_exec::call_user_scalar_function(
                    self.engine,
                    &call.name,
                    &arguments,
                )
                .ok_or_else(|| {
                    uqa_execution::ExecError::Other(format!(
                        "user function `{}` disappeared during scalar projection",
                        call.name
                    ))
                })??;
                return Ok(SetFunctionState::Scalar(value));
            }
            let result = crate::sql::plpgsql_exec::call_user_table_function(
                self.engine,
                &call.name,
                &arguments,
            )
            .ok_or_else(|| {
                uqa_execution::ExecError::Other(format!(
                    "user function `{}` disappeared during set projection",
                    call.name
                ))
            })??;
            let rows = crate::sql::from_rows::registered_table_function_rows(
                &call.name,
                result,
                None,
                &[],
            )?;
            return Ok(SetFunctionState::Set {
                rows: Box::new(rows.into_iter().map(Ok)),
                exhausted: false,
            });
        }

        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let context = crate::sql::from_rows::SourceEvalContext::new(
            self.engine,
            self.params,
            &hook,
            &hook,
            &self.ctes.scalar_subqueries,
        );
        let table_call = crate::sql::from_rows::TableFunctionCall::new(
            &call.name,
            &call.name,
            None,
            &call.args,
            None,
            &[],
            &[],
        );
        let rows = crate::sql::from_rows::build_table_function_row_stream_with_row(
            &context,
            table_call,
            Some(row),
        )?;
        Ok(SetFunctionState::Set {
            rows,
            exhausted: false,
        })
    }

    fn start_expansion(&self, input: OwnedPhysicalRow) -> ExecResult<SetExpansion> {
        let calls = self
            .plan
            .calls
            .iter()
            .map(|call| self.call_state(call, &input))
            .collect::<ExecResult<Vec<_>>>()?;
        let has_set = calls
            .iter()
            .any(|call| matches!(call, SetFunctionState::Set { .. }));
        Ok(SetExpansion {
            input,
            calls,
            has_set,
            scalar_emitted: false,
        })
    }

    fn next_projected(&mut self) -> ExecResult<Option<PhysicalRow>> {
        let Some(expansion) = self.expansion.as_mut() else {
            return Ok(None);
        };
        let Some(values) = expansion.next_values()? else {
            return Ok(None);
        };
        let evaluation_row = expansion.input.row.clone().append_values(values);
        if self.pass_through {
            let mut output = (0..expansion.input.schema.physical_width())
                .map(RowProjectionValue::InputSlot)
                .collect::<Vec<_>>();
            for (_, expression) in self
                .plan
                .projections
                .iter()
                .filter(|(_, expression)| !matches!(expression, ScalarExpr::Star))
            {
                if let Some(position) =
                    uqa_execution::order_expression_position(&self.evaluation_schema, expression)
                {
                    output.push(self.evaluation_schema.physical_slot(position).map_or(
                        RowProjectionValue::Owned(Value::Null),
                        RowProjectionValue::InputSlot,
                    ));
                } else {
                    output.push(RowProjectionValue::Owned(
                        self.evaluator.evaluate_physical(
                            expression,
                            &self.evaluation_schema,
                            &evaluation_row,
                        )?,
                    ));
                }
            }
            return Ok(Some(evaluation_row.project_with_values(output)));
        }

        let mut output = Vec::with_capacity(self.schema.len());
        for (_, expression) in &self.plan.projections {
            if matches!(expression, ScalarExpr::Star) {
                for (position, column) in expansion.input.schema.columns().iter().enumerate() {
                    if self.evaluator.star_column_visible(column) {
                        output.push(expansion.input.schema.physical_slot(position).map_or(
                            RowProjectionValue::Owned(Value::Null),
                            RowProjectionValue::InputSlot,
                        ));
                    }
                }
            } else if let Some(position) =
                uqa_execution::order_expression_position(&self.evaluation_schema, expression)
            {
                output.push(self.evaluation_schema.physical_slot(position).map_or(
                    RowProjectionValue::Owned(Value::Null),
                    RowProjectionValue::InputSlot,
                ));
            } else {
                output.push(RowProjectionValue::Owned(
                    self.evaluator.evaluate_physical(
                        expression,
                        &self.evaluation_schema,
                        &evaluation_row,
                    )?,
                ));
            }
        }
        Ok(Some(
            evaluation_row
                .project_with_values(output)
                .without_lock_origins(),
        ))
    }
}

impl PhysicalOperator for SetProjection<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.input = Vec::new().into_iter();
        self.expansion = None;
        self.exhausted = false;
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.exhausted && self.expansion.is_none() {
            return Ok(None);
        }
        let mut output = Vec::with_capacity(self.output_batch_size);
        while output.len() < self.output_batch_size {
            if let Some(row) = self.next_projected()? {
                output.push(row);
                continue;
            }
            self.expansion = None;
            if let Some(input) = self.next_input()? {
                self.expansion = Some(self.start_expansion(input)?);
            } else {
                self.exhausted = true;
                break;
            }
        }
        if output.is_empty() {
            return Ok(None);
        }
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), output)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.input = Vec::new().into_iter();
        self.expansion = None;
        self.exhausted = true;
        self.child.close()
    }
}
