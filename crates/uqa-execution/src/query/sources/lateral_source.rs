//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lateral source execution and correlated query-block remapping.

use super::{
    build_join_operator_with_ctes, eval_scalar, resolve_user_table_function,
    validate_table_function_column_definition, CteScope, PlanSubqueryArena, SQLError, SQLParam,
    ScalarEvalContext, ScalarExpr, SourceContext, SourcePlan, TableFunctionCall,
};

/// Enforce EXECUTE on each function of a lateral FROM item, which the join calls once per outer row, as `PostgreSQL` checks it when it initializes the scan, before any outer row is read.
pub(super) fn require_lateral_function_execute<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    source: &SourcePlan,
    input_schema: &crate::RowSchema,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    let calls = match source {
        SourcePlan::Function {
            name,
            binding,
            args,
            ..
        } => vec![(name.as_str(), binding.as_ref(), args.as_slice())],
        SourcePlan::FunctionGroup { functions, .. } => functions
            .iter()
            .map(|function| {
                (
                    function.name.as_str(),
                    function.binding.as_ref(),
                    function.args.as_slice(),
                )
            })
            .collect(),
        _ => return Ok(()),
    };
    let hook = context.relational.expression_scope(ctes.clone());
    for (name, binding, args) in calls {
        match resolve_user_table_function(
            context.relational.catalog,
            name,
            binding,
            args,
            input_schema,
            params,
            hook.as_ref(),
        )? {
            Some(resolved) => hook.require_user_function_execute(&resolved.function)?,
            None => {
                if let Some(binding) = binding.filter(|binding| binding.builtin) {
                    hook.require_builtin_execute(binding)?;
                }
            }
        }
        super::function_source::require_argument_function_execute(
            context.relational.catalog,
            hook.as_ref(),
            args,
            input_schema,
            params,
        )?;
    }
    Ok(())
}

pub(super) struct QueryLateralSource<'a, S: Clone + 'static> {
    pub(super) context: SourceContext<'a, S>,
    pub(super) right: SourcePlan,
    pub(super) on: Option<crate::scalar::PreparedExpressions<ScalarExpr>>,
    /// The planner's predicates on the right relation's qualifiers, bound to `right_schema`. The planner removes them from the query block's residual filter, so every produced right row must satisfy them. An error names a column they read that the relation does not produce.
    pub(super) right_filter: Option<
        Result<
            (
                crate::scalar::PreparedExpressions<ScalarExpr>,
                crate::SharedExpressionEvaluator<'a>,
            ),
            String,
        >,
    >,
    pub(super) function_arguments: Option<crate::scalar::PreparedExpressions<Vec<ScalarExpr>>>,
    pub(super) routine_descriptions:
        uqa_sql::routines::invocation::descriptions::RoutineCallDescriptions,
    pub(super) expression_runtime: std::sync::OnceLock<
        std::sync::Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a>,
    >,
    pub(super) params: &'a [SQLParam],
    pub(super) ctes: CteScope<S>,
    /// Calling join row followed by any enclosing-query values. Only the join's own row changes between calls.
    pub(super) input_schema: crate::RowSchema,
    pub(super) right_schema: crate::RowSchema,
    pub(super) pinned_right: Option<crate::OwnedPhysicalRow>,
}

impl<'a, S: Clone + Send + Sync + 'static> QueryLateralSource<'a, S> {
    fn expression_runtime(
        &self,
    ) -> std::sync::Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a> {
        if let Some(saved) = self.expression_runtime.get() {
            if saved.routine_description_scope().is_some() {
                return std::sync::Arc::clone(saved);
            }
        }
        let hook = self.context.relational.expression_scope(self.ctes.clone());
        if hook.routine_description_scope().is_some() {
            let _ = self.expression_runtime.set(std::sync::Arc::clone(&hook));
        }
        hook
    }
    fn function_rows(
        &self,
        left_row: &crate::OwnedPhysicalRow,
    ) -> crate::ExecResult<Option<crate::LateralRows<'a>>> {
        let SourcePlan::Function {
            name,
            binding,
            output_name,
            relations,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
        } = &self.right
        else {
            return Ok(None);
        };
        let hook = self.expression_runtime();
        let retained_function = binding
            .as_ref()
            .filter(|binding| !binding.builtin && left_row.schema == self.input_schema)
            .and_then(|binding| {
                let scope = hook.routine_description_scope()?;
                self.routine_descriptions.get(binding, &scope)?.function()
            });
        let resolved = if retained_function.is_some() {
            None
        } else {
            resolve_user_table_function(
                self.context.relational.catalog,
                name,
                binding.as_ref(),
                args,
                &left_row.schema,
                self.params,
                hook.as_ref(),
            )?
        };
        let selected_binding = resolved
            .as_ref()
            .map(|resolved| &resolved.binding)
            .or(binding.as_ref());
        // A successful fixed-source validation precedes publication of this
        // call's description. Explicit record definitions retain live lookup.
        if retained_function.is_none() || !column_types.is_empty() {
            validate_table_function_column_definition(
                self.context.relational.catalog,
                uqa_sql::semantics::ColumnDefinitionRequest {
                    name,
                    binding: binding.as_ref(),
                    user_function: retained_function
                        .as_deref()
                        .or_else(|| resolved.as_ref().map(|resolved| resolved.function.as_ref())),
                    user_invocation: selected_binding
                        .and_then(|binding| binding.invocation.as_deref()),
                    args,
                    declared_types: column_types,
                    column_names: column_aliases,
                },
                &left_row.schema,
                self.params,
                hook.as_ref(),
            )?;
        }
        let call = TableFunctionCall {
            name,
            binding: selected_binding,
            output_name,
            relations: relations.as_ref(),
            args: self
                .function_arguments
                .as_deref()
                .expect("lateral function arguments are prepared"),
            function_states: self
                .function_arguments
                .as_ref()
                .map(crate::scalar::PreparedExpressions::calls),
            routine_descriptions: Some(&self.routine_descriptions),
            alias: alias.as_deref(),
            column_aliases,
            ordinality: *ordinality,
            column_types,
        };
        let output = hook.table_function_rows(call, self.params, Some(left_row))?;
        let schema = self.right_schema.clone();
        if output.columns.len() != schema.len() {
            return Err(SQLError::Internal(format!(
                "lateral table function produced {} columns for a {}-column source",
                output.columns.len(),
                schema.len()
            ))
            .into());
        }
        Ok(Some(Box::new(output.rows.map(move |row| {
            row.map(|row| crate::OwnedPhysicalRow::new(schema.clone(), row))
        }))))
    }
}

impl<'a, S: Clone + Send + Sync + 'static> crate::LateralSource<'a> for QueryLateralSource<'a, S> {
    fn admits(&mut self, right: &crate::OwnedPhysicalRow) -> crate::ExecResult<bool> {
        let Some(filter) = self.right_filter.as_ref() else {
            return Ok(true);
        };
        let (predicate, evaluator) = filter.as_ref().map_err(|column| {
            crate::ExecError::SQL(SQLError::Internal(format!(
                "a predicate placed on a lateral relation reads {column}, which the relation does not produce"
            )))
        })?;
        Ok(uqa_sql::expr::truthy(
            &evaluator.evaluate_physical_with_function_states(
                predicate,
                &right.schema,
                &right.row,
                predicate.calls(),
            )?,
        ))
    }

    fn rows_for(
        &mut self,
        left_row: &crate::OwnedPhysicalRow,
    ) -> crate::ExecResult<crate::LateralRows<'a>> {
        if let Some(row) = self.pinned_right.as_ref() {
            return Ok(Box::new(std::iter::once(Ok(row.clone()))));
        }
        let combined;
        let left_row = if let Some(outer) = self.ctes.row_lock_outer_row() {
            combined = crate::OwnedPhysicalRow::new(
                self.input_schema.clone(),
                crate::PhysicalRow::concat_left_owned(left_row.row.clone(), &outer.row),
            );
            &combined
        } else {
            left_row
        };
        if let Some(rows) = self.function_rows(left_row)? {
            return Ok(rows);
        }
        let mut scoped_ctes = self.ctes.clone();
        scoped_ctes.set_row_lock_outer_row(left_row.clone());
        let operator = build_join_operator_with_ctes(
            &self.context,
            &self.right,
            self.params,
            &mut scoped_ctes,
            None,
            None,
        )?;
        let schema = self.right_schema.clone();
        let rows = crate::physical::demand::OperatorRows::open(operator)?;
        Ok(Box::new(rows.map(move |row| row?.relabel(schema.clone()))))
    }

    fn matches(&mut self, joined: &crate::OwnedPhysicalRow) -> crate::ExecResult<bool> {
        let Some(filter) = self.on.as_ref() else {
            return Ok(true);
        };
        let scoped_hook = self.expression_runtime();
        let subquery_arena =
            PlanSubqueryArena::new(&self.ctes.scalar_subqueries, Some(scoped_hook.as_ref()));
        let context = ScalarEvalContext::from_row_lookup(joined, self.params)
            .with_function_hook(scoped_hook.as_ref())
            .with_function_states(filter.calls())
            .with_subquery_runner(&subquery_arena)
            .with_physical_outer_row(&joined.schema, &joined.row);
        Ok(uqa_sql::expr::truthy(&eval_scalar(filter, &context)?))
    }
}
