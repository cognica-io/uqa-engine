//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent exact routine identity binding for catalog-owned query plans.

use super::{
    cte_references_own_name, expr_contains_subquery, extend_cte_generated_schema,
    extend_recursive_cte_binding_schema, operator_join_relation_schema, ordered_plan_ctes,
    overlay_outer_schema, rename_schema, CteScope, Engine, QueryPlan, RelationalPlan, RowSchema,
    SQLError, SQLParam, ScalarExpr, SchemaScope, SourcePlan,
};
use uqa_execution::FunctionTypeResolver;
use uqa_sql::ast::FunctionBinding;

impl SchemaScope {
    fn bind_query_routines_for_storage(
        &mut self,
        engine: &Engine,
        plan: &mut QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let ordered_names = ordered_plan_ctes(plan)?
            .into_iter()
            .map(|cte| cte.name.clone())
            .collect::<Vec<_>>();
        let mut previous = Vec::with_capacity(ordered_names.len());
        for name in ordered_names {
            let position = plan
                .ctes
                .iter()
                .position(|cte| cte.name == name)
                .ok_or_else(|| SQLError::Internal(format!("ordered CTE `{name}` disappeared")))?;
            let self_recursive = cte_references_own_name(&plan.ctes[position]);
            if let Some(cycle) = plan.ctes[position].cycle.as_mut() {
                let schema = RowSchema::default();
                self.bind_scalar_routines_for_storage(
                    engine,
                    &mut cycle.mark_value,
                    &schema,
                    &[],
                    params,
                    outer,
                )?;
                self.bind_scalar_routines_for_storage(
                    engine,
                    &mut cycle.mark_default,
                    &schema,
                    &[],
                    params,
                    outer,
                )?;
            }
            let provisional = if self_recursive {
                self.bind_recursive_seed(engine, &plan.ctes[position].query, params, outer)?
            } else {
                self.bind_query_routines_for_storage(
                    engine,
                    &mut plan.ctes[position].query,
                    params,
                    outer,
                )?
            };
            let columns = plan.ctes[position].columns.clone();
            let provisional = rename_schema(&provisional, &columns, None);
            let provisional = if self_recursive {
                extend_recursive_cte_binding_schema(
                    engine,
                    &plan.ctes[position],
                    provisional,
                    params,
                )?
            } else {
                extend_cte_generated_schema(engine, &plan.ctes[position], provisional, params)?
            };
            previous.push((name.clone(), self.ctes.insert(name.clone(), provisional)));
            if self_recursive {
                let complete = self.bind_query_routines_for_storage(
                    engine,
                    &mut plan.ctes[position].query,
                    params,
                    outer,
                )?;
                let complete = rename_schema(&complete, &columns, None);
                let complete =
                    extend_cte_generated_schema(engine, &plan.ctes[position], complete, params)?;
                self.ctes.insert(name, complete);
            }
        }

        let result = self.bind_root_routines_for_storage(engine, &mut plan.root, params, outer);
        for (name, schema) in previous.into_iter().rev() {
            match schema {
                Some(schema) => {
                    self.ctes.insert(name, schema);
                }
                None => {
                    self.ctes.remove(&name);
                }
            }
        }
        result
    }

    fn bind_root_routines_for_storage(
        &mut self,
        engine: &Engine,
        root: &mut RelationalPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match root {
            RelationalPlan::QueryBlock(block) => {
                let source_schema = match block.from.as_mut() {
                    Some(source) => self.bind_source_for_execution(
                        engine,
                        source,
                        &block.subqueries,
                        params,
                        outer,
                    )?,
                    None => RowSchema::default(),
                };
                if let Some(source) = block.from.as_mut() {
                    self.bind_source_routines_for_storage(
                        engine,
                        source,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                let expression_schema = overlay_outer_schema(&source_schema, outer);
                for subquery in &mut block.subqueries {
                    self.bind_query_routines_for_storage(
                        engine,
                        subquery,
                        params,
                        Some(&expression_schema),
                    )?;
                }
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.bind_query_routines_for_storage(engine, left, params, outer)?;
                self.bind_query_routines_for_storage(engine, right, params, outer)?;
                for subquery in subqueries {
                    self.bind_query_routines_for_storage(engine, subquery, params, outer)?;
                }
            }
            RelationalPlan::Values { subqueries, .. } => {
                for subquery in subqueries {
                    self.bind_query_routines_for_storage(engine, subquery, params, outer)?;
                }
            }
        }

        let set_output = match &*root {
            RelationalPlan::SetOp { .. } => {
                Some(self.bind_root(engine, root, params, outer, false)?)
            }
            RelationalPlan::QueryBlock(_) | RelationalPlan::Values { .. } => None,
        };
        match root {
            RelationalPlan::QueryBlock(block) => {
                if block.from.is_none()
                    && block
                        .projections
                        .iter()
                        .any(|projection| matches!(projection.expr, ScalarExpr::Star))
                    && outer.is_none()
                {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: "SELECT * with no tables specified is not valid".into(),
                    });
                }
                let source_schema = block.from.as_ref().map_or_else(
                    || Ok(RowSchema::default()),
                    |source| self.bind_source(engine, source, &block.subqueries, params, outer),
                )?;
                let expression_schema = overlay_outer_schema(&source_schema, outer);
                if let Some(filter) = block.r#where.as_mut() {
                    self.bind_scalar_routines_for_storage(
                        engine,
                        filter,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                for projection in &mut block.projections {
                    self.bind_scalar_routines_for_storage(
                        engine,
                        &mut projection.expr,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                block.projections = crate::sql::select::expand_bound_projection_stars(
                    &block.projections,
                    &source_schema,
                )?;
                for expression in &mut block.group_by {
                    self.bind_scalar_routines_for_storage(
                        engine,
                        expression,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                for set in &mut block.grouping_sets {
                    for expression in set {
                        self.bind_scalar_routines_for_storage(
                            engine,
                            expression,
                            &expression_schema,
                            &block.subqueries,
                            params,
                            outer,
                        )?;
                    }
                }
                if let Some(having) = block.having.as_mut() {
                    self.bind_scalar_routines_for_storage(
                        engine,
                        having,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                for order in &mut block.order_by {
                    self.bind_scalar_routines_for_storage(
                        engine,
                        &mut order.expr,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                if let Some(limit) = block.limit.as_mut() {
                    self.bind_scalar_routines_for_storage(
                        engine,
                        limit,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                if let Some(offset) = block.offset.as_mut() {
                    self.bind_scalar_routines_for_storage(
                        engine,
                        offset,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                for expression in &mut block.distinct_on {
                    self.bind_scalar_routines_for_storage(
                        engine,
                        expression,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
            }
            RelationalPlan::SetOp {
                order_by,
                limit,
                offset,
                subqueries,
                ..
            } => {
                let output = set_output
                    .as_ref()
                    .expect("set-operation output schema was bound before routine expressions");
                for order in order_by {
                    self.bind_scalar_routines_for_storage(
                        engine,
                        &mut order.expr,
                        output,
                        subqueries,
                        params,
                        outer,
                    )?;
                }
                if let Some(limit) = limit {
                    self.bind_scalar_routines_for_storage(
                        engine, limit, output, subqueries, params, outer,
                    )?;
                }
                if let Some(offset) = offset {
                    self.bind_scalar_routines_for_storage(
                        engine, offset, output, subqueries, params, outer,
                    )?;
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                let input = outer.cloned().unwrap_or_default();
                for expression in rows.iter_mut().flatten() {
                    self.bind_scalar_routines_for_storage(
                        engine, expression, &input, subqueries, params, outer,
                    )?;
                }
            }
        }
        self.bind_root(engine, root, params, outer, false)
    }

    fn bind_source_routines_for_storage(
        &mut self,
        engine: &Engine,
        source: &mut SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        match source {
            SourcePlan::Join {
                left,
                right,
                on,
                lateral,
                ..
            } => {
                self.bind_source_routines_for_storage(engine, left, subqueries, params, outer)?;
                let left_schema = self.bind_source(engine, left, subqueries, params, outer)?;
                let implicit_lateral_function = matches!(
                    right.as_ref(),
                    SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
                );
                let right_scope = (*lateral || implicit_lateral_function)
                    .then(|| overlay_outer_schema(&left_schema, outer));
                let right_outer = right_scope.as_ref().or(outer);
                self.bind_source_routines_for_storage(
                    engine,
                    right,
                    subqueries,
                    params,
                    right_outer,
                )?;
                if let Some(on) = on {
                    let right_schema =
                        self.bind_source(engine, right, subqueries, params, right_outer)?;
                    let input = RowSchema::join(&left_schema, &right_schema, std::iter::empty());
                    let input = overlay_outer_schema(&input, outer);
                    self.bind_scalar_routines_for_storage(
                        engine, on, &input, subqueries, params, outer,
                    )?;
                }
                Ok(())
            }
            SourcePlan::Subquery { body, .. } => self
                .bind_query_routines_for_storage(engine, body, params, outer)
                .map(|_| ()),
            SourcePlan::Values { rows, .. } => {
                let input = outer.cloned().unwrap_or_default();
                for expression in rows.iter_mut().flatten() {
                    self.bind_scalar_routines_for_storage(
                        engine, expression, &input, subqueries, params, outer,
                    )?;
                }
                Ok(())
            }
            SourcePlan::Function {
                name,
                relation,
                args,
                ..
            } => {
                let local = crate::sql::builtin_function_dispatch_name(name);
                let input = if crate::operator_tree_bridge::is_operator_join_table_function(&local)
                {
                    operator_join_relation_schema(
                        engine,
                        relation.as_deref(),
                        self.catalog_tables_only,
                    )?
                } else {
                    outer.cloned().unwrap_or_default()
                };
                for expression in args {
                    self.bind_scalar_routines_for_storage(
                        engine, expression, &input, subqueries, params, outer,
                    )?;
                }
                Ok(())
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    let local = crate::sql::builtin_function_dispatch_name(&function.name);
                    let input =
                        if crate::operator_tree_bridge::is_operator_join_table_function(&local) {
                            operator_join_relation_schema(
                                engine,
                                function.relation.as_deref(),
                                self.catalog_tables_only,
                            )?
                        } else {
                            outer.cloned().unwrap_or_default()
                        };
                    for expression in &mut function.args {
                        self.bind_scalar_routines_for_storage(
                            engine, expression, &input, subqueries, params, outer,
                        )?;
                    }
                }
                Ok(())
            }
            SourcePlan::Table { .. } => Ok(()),
        }
    }

    fn bind_scalar_routines_for_storage(
        &mut self,
        engine: &Engine,
        expression: &mut ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let mut failure = None;
        uqa_planner::rewrite_scalar_expression(expression, &mut |expression| {
            if failure.is_some() {
                return;
            }
            let ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } = expression
            else {
                return;
            };
            if binding
                .as_ref()
                .and_then(|binding| binding.dispatch)
                .is_some()
            {
                return;
            }
            if let Err(error) = self.bind_scalar_function_for_storage(
                engine, name, binding, args, schema, subqueries, params, outer,
            ) {
                failure = Some(error);
            }
        });
        failure.map_or(Ok(()), Err)
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_scalar_function_for_storage(
        &mut self,
        engine: &Engine,
        name: &str,
        binding: &mut Option<FunctionBinding>,
        args: &[ScalarExpr],
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let resolver = self.query_function_type_resolver_for_subqueries(
            engine,
            args.iter().any(expr_contains_subquery),
            schema,
            subqueries,
            params,
            outer,
        )?;
        let resolver = resolver
            .as_ref()
            .map_or(engine as &dyn FunctionTypeResolver, |resolver| resolver);
        let (argument_names, argument_types, explicit_variadic) =
            uqa_execution::function_call_argument_signature(args, schema, params, Some(resolver))?;
        let selected = if uqa_execution::is_fixed_builtin(name) {
            uqa_execution::resolve_fixed_builtin_call(
                name,
                binding.as_ref(),
                &argument_names,
                &argument_types,
                explicit_variadic,
                Some(resolver),
            )?
            .map(|resolved| resolved.selected)
        } else {
            resolver.resolve_function_overload(
                name,
                binding.as_ref(),
                &argument_names,
                &argument_types,
                explicit_variadic,
            )?
        };
        if let Some(selected) = selected {
            *binding = Some(selected.binding);
        }
        Ok(())
    }
}

pub(in crate::sql) fn bind_query_plan_routines_for_storage(
    engine: &Engine,
    plan: &mut QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_analysis(ctes).bind_query_routines_for_storage(engine, plan, params, outer)
}
