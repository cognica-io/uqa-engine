//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical set-returning projection operator.

use crate::RowProjectionValue;

use super::{
    eval_call_arguments, Arc, Batch, ColumnType, ExecResult, OwnedPhysicalRow, PhysicalOperator,
    PhysicalRow, PlanSubqueryArena, ProjectionTarget, RowSchema, SQLError, SQLParam,
    ScalarEvalContext, ScalarExpr, SetExpansion, SetFunctionCall, SetFunctionRuntime,
    SetFunctionState, SetProjectionPlan, SharedExpressionEvaluator, Value,
};

pub struct SetProjection<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    runtime: Arc<dyn SetFunctionRuntime + 'a>,
    params: &'a [SQLParam],
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
    resolver: &dyn crate::FunctionTypeResolver,
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
    crate::scalar_type_with_resolver(&expression, input_schema, params, resolver)
        .ok()
        .flatten()
}

impl<'a> SetProjection<'a> {
    pub fn from_plan(
        child: Box<dyn PhysicalOperator + 'a>,
        runtime: Arc<dyn SetFunctionRuntime + 'a>,
        params: &'a [SQLParam],
        evaluator: SharedExpressionEvaluator<'a>,
        plan: SetProjectionPlan,
        pass_through: bool,
        output_batch_size: usize,
    ) -> Self {
        let projections = &plan.projections;
        let resolver = runtime.as_ref();
        let call_types = plan
            .calls
            .iter()
            .map(|call| set_call_output_type(resolver, call, child.row_schema(), params))
            .collect::<Vec<_>>();
        let appended = projections
            .iter()
            .zip(&call_types)
            .filter(|((_, expression), _)| !matches!(expression, ScalarExpr::Star))
            .map(|((target, _), ty)| {
                let ProjectionTarget::Internal(column) = target else {
                    unreachable!("set-call expansion target must be an internal attribute");
                };
                (*column, ty.clone())
            })
            .collect::<Vec<_>>();
        let schema = if pass_through {
            RowSchema::append_internal_typed(child.row_schema(), &appended)
        } else {
            unreachable!("set-call expansion always preserves its input")
        };
        let evaluation_columns = plan
            .calls
            .iter()
            .zip(call_types)
            .map(|(call, ty)| (call.placeholder, ty))
            .collect::<Vec<_>>();
        let evaluation_schema =
            RowSchema::append_internal_typed(child.row_schema(), &evaluation_columns);
        let output_batch_size = output_batch_size.max(1);
        Self {
            child,
            runtime,
            params,
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
        if uqa_sql::semantics::builtin_function_dispatch_name(&identity) == "unnest"
            && call.args.len() != 1
            && call.binding.as_ref().is_none_or(|binding| binding.builtin)
        {
            return Err(SQLError::UnknownFunction(
                "unnest with multiple arrays is only valid in FROM".into(),
            )
            .into());
        }
        let runtime = self.runtime.as_ref();
        let binding = call.binding.as_ref();
        let has_sql_function = runtime.has_user_function(&call.name, binding)?;
        if !runtime.has_registered_table_function(&identity) && has_sql_function {
            let subqueries = PlanSubqueryArena::new(runtime.subquery_plans(), Some(runtime));
            let context = ScalarEvalContext::from_row_lookup(row, self.params)
                .with_function_hook(runtime)
                .with_subquery_runner(&subqueries)
                .with_physical_outer_row(&row.schema, &row.row);
            let arguments = eval_call_arguments(&call.args, &context)?;
            let returns_set = runtime
                .user_function_returns_set(&call.name, binding, &arguments)
                .ok_or_else(|| {
                    crate::ExecError::Other(format!(
                        "user function `{}` disappeared during projection",
                        call.name
                    ))
                })??;
            if !returns_set {
                let value = runtime
                    .call_user_scalar_function(&call.name, binding, &arguments)
                    .ok_or_else(|| {
                        crate::ExecError::Other(format!(
                            "user function `{}` disappeared during scalar projection",
                            call.name
                        ))
                    })??;
                return Ok(SetFunctionState::Scalar(value));
            }
            let result = runtime
                .call_user_table_function(&call.name, binding, &arguments)
                .ok_or_else(|| {
                    crate::ExecError::Other(format!(
                        "user function `{}` disappeared during set projection",
                        call.name
                    ))
                })??;
            let output = super::registered_table_function_rows(&call.name, result, None, &[])?;
            return Ok(SetFunctionState::Set {
                columns: output.columns,
                rows: output.rows,
                exhausted: false,
            });
        }
        let table_call = super::TableFunctionCall {
            name: &call.name,
            binding,
            output_name: &call.name,
            relations: None,
            args: &call.args,
            alias: None,
            column_aliases: &[],
            ordinality: false,
            column_types: &[],
        };
        let output = runtime.table_function_rows(table_call, self.params, Some(row))?;
        Ok(SetFunctionState::Set {
            columns: output.columns,
            rows: output.rows,
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
                    crate::order_expression_position(&self.evaluation_schema, expression)
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
                for position in 0..expansion.input.schema.len() {
                    if self
                        .evaluator
                        .star_position_visible(&expansion.input.schema, position)
                    {
                        output.push(expansion.input.schema.physical_slot(position).map_or(
                            RowProjectionValue::Owned(Value::Null),
                            RowProjectionValue::InputSlot,
                        ));
                    }
                }
            } else if let Some(position) =
                crate::order_expression_position(&self.evaluation_schema, expression)
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
