//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lateral source execution and correlated query-block remapping.

use super::{
    build_join_operator_with_ctes, eval_scalar, query_output_shared, resolve_user_table_function,
    validate_table_function_column_definition, CteScope, PlanSubqueryArena, QueryOutputMode,
    SQLError, SQLParam, ScalarEvalContext, ScalarExpr, SourceContext, SourcePlan,
    TableFunctionCall,
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
    pub(super) params: &'a [SQLParam],
    pub(super) ctes: CteScope<S>,
    pub(super) right_schema: crate::RowSchema,
    pub(super) pinned_right: Option<crate::OwnedPhysicalRow>,
}

impl<S: Clone + Send + Sync + 'static> crate::LateralSource for QueryLateralSource<'_, S> {
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

    #[expect(
        clippy::too_many_lines,
        reason = "preserves source schema and row identity"
    )]
    fn rows_for(
        &mut self,
        left_row: &crate::OwnedPhysicalRow,
    ) -> crate::ExecResult<crate::LateralRows> {
        if let Some(row) = self.pinned_right.as_ref() {
            return Ok(Box::new(std::iter::once(Ok(row.clone()))));
        }
        if matches!(&self.right, SourcePlan::FunctionGroup { .. }) {
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
            let columns = operator.schema().to_vec();
            let output = crate::query::collection::collect_query_operator(
                self.context.relational.runtime,
                columns,
                operator,
                QueryOutputMode::SharedSpill,
            )?;
            let rows = query_output_shared(output, "lateral function group")?;
            let schema = self.right_schema.clone();
            return Ok(Box::new(
                rows.read_rows()?
                    .map(move |row| row?.relabel(schema.clone())),
            ));
        }
        if let SourcePlan::Function {
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
        {
            let hook = self.context.relational.expression_scope(self.ctes.clone());
            let resolved = resolve_user_table_function(
                self.context.relational.catalog,
                name,
                binding.as_ref(),
                args,
                &left_row.schema,
                self.params,
                hook.as_ref(),
            )?;
            validate_table_function_column_definition(
                self.context.relational.catalog,
                uqa_sql::semantics::ColumnDefinitionRequest {
                    name,
                    binding: binding.as_ref(),
                    user_function: resolved.as_ref().map(|resolved| resolved.function.as_ref()),
                    user_invocation: resolved
                        .as_ref()
                        .and_then(|resolved| resolved.binding.invocation.as_deref()),
                    args,
                    declared_types: column_types,
                    column_names: column_aliases,
                },
                &left_row.schema,
                self.params,
                hook.as_ref(),
            )?;
            let call = TableFunctionCall {
                name,
                binding: resolved
                    .as_ref()
                    .map(|resolved| &resolved.binding)
                    .or(binding.as_ref()),
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
            return Ok(Box::new(output.rows.map(move |row| {
                row.map(|row| crate::OwnedPhysicalRow::new(schema.clone(), row))
            })));
        }
        match &self.right {
            SourcePlan::Subquery { body, .. } => {
                let output = self.context.ctes.queries.execute_lateral_query(
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
            SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. } => {
                Err(crate::ExecError::SQL(SQLError::Internal(
                    "function source reached the relational-source fallback".into(),
                )))
            }
            source => {
                let operator = build_join_operator_with_ctes(
                    &self.context,
                    source,
                    self.params,
                    &mut self.ctes,
                    None,
                    None,
                )?;
                let columns = operator.schema().to_vec();
                let output = crate::query::collection::collect_query_operator(
                    self.context.relational.runtime,
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

    fn matches(&mut self, joined: &crate::OwnedPhysicalRow) -> crate::ExecResult<bool> {
        let Some(filter) = self.on.as_ref() else {
            return Ok(true);
        };
        let scoped_hook = self.context.relational.expression_scope(self.ctes.clone());
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
