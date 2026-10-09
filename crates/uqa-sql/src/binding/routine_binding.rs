//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent exact routine identity binding for catalog-owned query plans.

use super::{
    cte_references_own_name, extend_cte_generated_schema, extend_recursive_cte_binding_schema,
    operator_join_relation_schemas, overlay_outer_schema, rename_schema, BindingContext,
    ColumnType, QueryPlan, RelationalPlan, RowSchema, SQLError, SQLParam, ScalarExpr, SchemaScope,
    SourcePlan,
};
use crate::ast::FunctionBinding;
use crate::plan::ExpressionPlan;
use crate::routines::RoutineResolution;
use crate::{ColumnIdentity, FunctionTypeResolver};

impl SchemaScope {
    pub(super) fn with_stored_outer_internal_aliases(&self, schema: &RowSchema) -> RowSchema {
        self.stored_expression_outer.as_ref().map_or_else(
            || schema.clone(),
            |outer| {
                if schema.physical_width() < outer.physical_width() {
                    schema.clone()
                } else {
                    RowSchema::with_trailing_internal_aliases(schema, outer)
                }
            },
        )
    }

    fn canonicalize_stored_outer_columns(&self, expression: &mut ScalarExpr, schema: &RowSchema) {
        let Some(outer) = self.stored_expression_outer.as_ref() else {
            return;
        };
        let Some(outer_start) = schema.physical_width().checked_sub(outer.physical_width()) else {
            return;
        };
        crate::plan::rewrite_scalar_expression(expression, &mut |node| {
            let lookup = match node {
                ScalarExpr::Column(column) => ColumnIdentity::unqualified(column.as_str()),
                ScalarExpr::QualifiedColumn { qualifier, column } => {
                    ColumnIdentity::qualified(qualifier.as_str(), column.as_str())
                }
                _ => return,
            };
            let Some(slot) = schema.physical_slot_for_identity(&lookup) else {
                return;
            };
            if slot < outer_start {
                return;
            }
            let outer_slot = slot - outer_start;
            let Some(column) = outer.unique_internal_column_for_slot(outer_slot) else {
                return;
            };
            *node = ScalarExpr::InternalColumn(column);
        });
    }

    fn bind_cte_routines_for_storage(
        &mut self,
        routines: &dyn RoutineResolution,
        body: &mut crate::plan::CtePlanBody,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let crate::plan::CtePlanBody::Command(command) = body else {
            return self.bind_query_routines_for_storage(
                routines,
                body.query_mut().expect("query CTE body"),
                params,
                outer,
            );
        };
        self.bind_command_routines_for_storage(routines, command, params, outer)?;
        self.bind_command_returning(routines, command, params)
    }

    pub(super) fn bind_cte_routine_schemas(
        &mut self,
        routines: &dyn RoutineResolution,
        ctes: &mut [crate::plan::CtePlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<Vec<(String, bool, Option<RowSchema>)>, SQLError> {
        let ordered_names = crate::semantics::ordered_cte_plans(ctes)?
            .into_iter()
            .map(|cte| cte.name.clone())
            .collect::<Vec<_>>();
        let mut previous = Vec::with_capacity(ordered_names.len());
        for name in ordered_names {
            let position = ctes
                .iter()
                .position(|cte| cte.name == name)
                .ok_or_else(|| SQLError::Internal(format!("ordered CTE `{name}` disappeared")))?;
            let self_recursive = cte_references_own_name(&ctes[position]);
            if let Some(cycle) = ctes[position].cycle.as_mut() {
                let schema = RowSchema::default();
                self.bind_scalar_routines_for_storage(
                    routines,
                    &mut cycle.mark_value,
                    &schema,
                    &[],
                    params,
                )?;
                self.bind_scalar_routines_for_storage(
                    routines,
                    &mut cycle.mark_default,
                    &schema,
                    &[],
                    params,
                )?;
            }
            let provisional = if self_recursive {
                self.bind_recursive_seed(
                    routines,
                    ctes[position]
                        .body
                        .query()
                        .ok_or_else(|| SQLError::Routine {
                            sqlstate: "42P19".into(),
                            message: format!(
                                "recursive query \"{}\" must not contain data-modifying statements",
                                ctes[position].name
                            ),
                        })?,
                    params,
                    outer,
                )?
            } else {
                self.bind_cte_routines_for_storage(
                    routines,
                    &mut ctes[position].body,
                    params,
                    outer,
                )?
            };
            let columns = ctes[position].columns.clone();
            let provisional = rename_schema(&provisional, &columns, None);
            let provisional = if self_recursive {
                extend_recursive_cte_binding_schema(routines, &ctes[position], provisional, params)?
            } else {
                extend_cte_generated_schema(routines, &ctes[position], provisional, params)?
            };
            previous.push((
                name.clone(),
                self.set_cte_returning(&ctes[position]),
                self.ctes.insert(name.clone(), provisional),
            ));
            if self_recursive {
                let complete = self.bind_cte_routines_for_storage(
                    routines,
                    &mut ctes[position].body,
                    params,
                    outer,
                )?;
                let complete = rename_schema(&complete, &columns, None);
                let complete =
                    extend_cte_generated_schema(routines, &ctes[position], complete, params)?;
                self.ctes.insert(name, complete);
            }
        }

        Ok(previous)
    }

    pub(super) fn bind_query_routines_for_storage(
        &mut self,
        routines: &dyn RoutineResolution,
        plan: &mut QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let previous = self.bind_cte_routine_schemas(routines, &mut plan.ctes, params, outer)?;

        let result = self.bind_root_routines_for_storage(routines, &mut plan.root, params, outer);
        self.restore_cte_schemas(previous);
        result
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    fn bind_root_routines_for_storage(
        &mut self,
        routines: &dyn RoutineResolution,
        root: &mut RelationalPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match root {
            RelationalPlan::QueryBlock(block) => {
                // The sources' own expressions resolve before the sources are analyzed, so that an analysis sees the names a nested query resolved, such as its output names in GROUP BY.
                if let Some(source) = block.from.as_mut() {
                    self.bind_source_routines_for_storage(
                        routines,
                        source,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                let source_schema = match block.from.as_mut() {
                    Some(source) => self.bind_source_for_execution(
                        routines,
                        source,
                        &block.subqueries,
                        params,
                        outer,
                    )?,
                    None => RowSchema::default(),
                };
                let expression_schema = overlay_outer_schema(&source_schema, outer);
                for subquery in &mut block.subqueries {
                    self.bind_query_routines_for_storage(
                        routines,
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
                self.bind_query_routines_for_storage(routines, left, params, outer)?;
                self.bind_query_routines_for_storage(routines, right, params, outer)?;
                for subquery in subqueries {
                    self.bind_query_routines_for_storage(routines, subquery, params, outer)?;
                }
            }
            RelationalPlan::Values { subqueries, .. } => {
                for subquery in subqueries {
                    self.bind_query_routines_for_storage(routines, subquery, params, outer)?;
                }
            }
        }

        let set_output = match &*root {
            RelationalPlan::SetOp { .. } => {
                Some(self.bind_root(routines, root, params, outer, false)?)
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
                    |source| self.bind_source(routines, source, &block.subqueries, params, outer),
                )?;
                let expression_schema = overlay_outer_schema(&source_schema, outer);
                if let Some(filter) = block.r#where.as_mut() {
                    self.bind_scalar_routines_for_storage(
                        routines,
                        filter,
                        &expression_schema,
                        &block.subqueries,
                        params,
                    )?;
                }
                let labels = super::routine_parameters::column_labels(&block.projections);
                for projection in &mut block.projections {
                    self.bind_scalar_routines_for_storage(
                        routines,
                        &mut projection.expr,
                        &expression_schema,
                        &block.subqueries,
                        params,
                    )?;
                }
                super::routine_parameters::keep_column_labels(&mut block.projections, labels);
                if self.preserve_syntax_shape {
                    // Stored syntax keeps `*` and output-name references as written; expanding a copy still reports their errors.
                    let mut expanded = (**block).clone();
                    expanded.projections = crate::semantics::expand_bound_projection_stars(
                        &block.projections,
                        &source_schema,
                    )?;
                    self.bind_grouping_variable_sites(&mut expanded, &source_schema);
                    crate::semantics::grouping_sets::bind_grouping_names(
                        routines,
                        &mut expanded,
                        &source_schema,
                        None,
                        params,
                    )?;
                } else {
                    block.projections = crate::semantics::expand_bound_projection_stars(
                        &block.projections,
                        &source_schema,
                    )?;
                    self.bind_grouping_variable_sites(block, &source_schema);
                    crate::semantics::grouping_sets::bind_grouping_names(
                        routines,
                        block,
                        &source_schema,
                        None,
                        params,
                    )?;
                }
                let output_names = self.output_names(&block.projections);
                for expression in block
                    .group_by
                    .iter_mut()
                    .chain(block.grouping_sets.iter_mut().flatten())
                {
                    // A preserved GROUP BY output name wins over a routine parameter when no local input column takes it. Its expression was already validated in the expanded copy.
                    if self.preserve_syntax_shape
                        && matches!(expression, ScalarExpr::Column(name)
                            if !source_schema.has_unqualified_column(name)
                                && !source_schema.column_is_ambiguous(name)
                                && output_names.contains(name))
                    {
                        continue;
                    }
                    self.bind_scalar_routines_for_storage(
                        routines,
                        expression,
                        &expression_schema,
                        &block.subqueries,
                        params,
                    )?;
                }
                if let Some(having) = block.having.as_mut() {
                    self.bind_scalar_routines_for_storage(
                        routines,
                        having,
                        &expression_schema,
                        &block.subqueries,
                        params,
                    )?;
                }
                // A bare name in ORDER BY or DISTINCT ON names an output column before any input column or parameter, as `findTargetlistEntrySQL92` resolves it.
                let names_output = |expression: &ScalarExpr| match expression {
                    ScalarExpr::Column(name) => output_names.contains(name),
                    _ => false,
                };
                for order in &mut block.order_by {
                    if names_output(&order.expr)
                        || self.output_takes_variable_site(&order.expr, &output_names)
                    {
                        continue;
                    }
                    self.bind_scalar_routines_for_storage(
                        routines,
                        &mut order.expr,
                        &expression_schema,
                        &block.subqueries,
                        params,
                    )?;
                }
                if let Some(limit) = block.limit.as_mut() {
                    self.bind_scalar_routines_for_storage(
                        routines,
                        limit,
                        &expression_schema,
                        &block.subqueries,
                        params,
                    )?;
                }
                if let Some(offset) = block.offset.as_mut() {
                    self.bind_scalar_routines_for_storage(
                        routines,
                        offset,
                        &expression_schema,
                        &block.subqueries,
                        params,
                    )?;
                }
                for expression in &mut block.distinct_on {
                    if names_output(expression)
                        || self.output_takes_variable_site(expression, &output_names)
                    {
                        continue;
                    }
                    self.bind_scalar_routines_for_storage(
                        routines,
                        expression,
                        &expression_schema,
                        &block.subqueries,
                        params,
                    )?;
                }
                for expression in block
                    .windows
                    .iter_mut()
                    .flat_map(|window| window.spec.expressions_mut())
                {
                    self.bind_scalar_routines_for_storage(
                        routines,
                        expression,
                        &expression_schema,
                        &block.subqueries,
                        params,
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
                let output = overlay_outer_schema(
                    set_output
                        .as_ref()
                        .expect("set-operation output schema was bound before routine expressions"),
                    outer,
                );
                let output = &output;
                for order in order_by {
                    self.bind_scalar_routines_for_storage(
                        routines,
                        &mut order.expr,
                        output,
                        subqueries,
                        params,
                    )?;
                }
                if let Some(limit) = limit {
                    self.bind_scalar_routines_for_storage(
                        routines, limit, output, subqueries, params,
                    )?;
                }
                if let Some(offset) = offset {
                    self.bind_scalar_routines_for_storage(
                        routines, offset, output, subqueries, params,
                    )?;
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                let input = outer.cloned().unwrap_or_default();
                for expression in rows.iter_mut().flatten() {
                    self.bind_scalar_routines_for_storage(
                        routines, expression, &input, subqueries, params,
                    )?;
                }
            }
        }
        self.bind_root(routines, root, params, outer, false)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    pub(super) fn bind_source_routines_for_storage(
        &mut self,
        engine: &dyn RoutineResolution,
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
                    self.bind_scalar_routines_for_storage(engine, on, &input, subqueries, params)?;
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
                        engine, expression, &input, subqueries, params,
                    )?;
                }
                Ok(())
            }
            SourcePlan::Function {
                name,
                relations,
                args,
                ..
            } => {
                let local = crate::semantics::builtin_function_dispatch_name(name);
                if crate::registry::is_operator_join_table_function(&local) {
                    let (left, right) = operator_join_relation_schemas(
                        &self.catalog,
                        &self.resolution,
                        relations.as_ref(),
                    )?;
                    let constant = RowSchema::default();
                    for (position, expression) in args.iter_mut().enumerate() {
                        let input = match position {
                            0 => &left,
                            1 => &right,
                            _ => &constant,
                        };
                        self.bind_scalar_routines_for_storage(
                            engine, expression, input, subqueries, params,
                        )?;
                    }
                    return Ok(());
                }
                let input = outer.cloned().unwrap_or_default();
                for expression in args {
                    self.bind_scalar_routines_for_storage(
                        engine, expression, &input, subqueries, params,
                    )?;
                }
                Ok(())
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    let local = crate::semantics::builtin_function_dispatch_name(&function.name);
                    if crate::registry::is_operator_join_table_function(&local) {
                        let (left, right) = operator_join_relation_schemas(
                            &self.catalog,
                            &self.resolution,
                            function.relations.as_ref(),
                        )?;
                        let constant = RowSchema::default();
                        for (position, expression) in function.args.iter_mut().enumerate() {
                            let input = match position {
                                0 => &left,
                                1 => &right,
                                _ => &constant,
                            };
                            self.bind_scalar_routines_for_storage(
                                engine, expression, input, subqueries, params,
                            )?;
                        }
                        continue;
                    }
                    let input = outer.cloned().unwrap_or_default();
                    for expression in &mut function.args {
                        self.bind_scalar_routines_for_storage(
                            engine, expression, &input, subqueries, params,
                        )?;
                    }
                }
                Ok(())
            }
            SourcePlan::Table { .. } => Ok(()),
        }
    }

    pub(super) fn bind_scalar_routines_for_storage(
        &mut self,
        engine: &dyn RoutineResolution,
        expression: &mut ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        let schema = self.with_stored_outer_internal_aliases(schema);
        let schema = &schema;
        self.canonicalize_stored_outer_columns(expression, schema);
        self.expand_stored_whole_rows(expression, schema);
        self.canonicalize_routine_parameters(expression, schema);
        self.resolve_variable_sites(expression, schema);
        match self.scalar_binding {
            super::ScalarBindingMode::References => return Ok(()),
            super::ScalarBindingMode::CompositeInputs => {
                return self.retain_composite_inputs_in_scope(
                    engine, expression, schema, subqueries, params,
                )
            }
            super::ScalarBindingMode::Stored => {}
        }
        let mut failure = None;
        crate::plan::rewrite_scalar_expression(expression, &mut |expression| {
            if failure.is_some() {
                return;
            }
            if matches!(expression, ScalarExpr::Func { order_syntax, name, binding, order_by, .. }
                if super::ordered_calls::uses_ordered_arguments(*order_syntax, name, binding.as_ref(), order_by.len())
                    || super::ordered_calls::is_ordered_set(name))
            {
                if let Err(error) = self.bind_ordered_function_for_storage(
                    engine, expression, schema, subqueries, params,
                ) {
                    failure = Some(error);
                }
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
            if let Some(dispatch) = binding.as_ref().and_then(|binding| binding.dispatch) {
                if let crate::ast::FunctionDispatch::NumericOperator(operator) = dispatch {
                    let selected = (|| {
                        let resolver = self.query_function_type_resolver_for_subqueries(
                            engine, args, schema, subqueries, params,
                        )?;
                        let (_, types, _) = crate::function_call_argument_signature(
                            args,
                            schema,
                            params,
                            Some(&resolver),
                        )?;
                        crate::type_resolution::numeric_operator_types(operator, &types)
                    })();
                    match selected {
                        Ok(selected) => {
                            binding
                                .as_mut()
                                .expect("structural operator binding")
                                .argument_types = selected
                                .arguments
                                .iter()
                                .map(crate::ColumnType::sql_name)
                                .collect();
                        }
                        Err(error) => failure = Some(error),
                    }
                }
                return;
            }
            if let Err(error) = self.bind_scalar_function_for_storage(
                engine, name, binding, args, schema, subqueries, params,
            ) {
                failure = Some(error);
            }
        });
        failure.map_or(Ok(()), Err)?;
        self.bind_stored_scalar_types(engine, expression, schema, subqueries, params)?;
        self.record_composite_dependencies(engine, expression, schema, subqueries, params)
    }

    /// Name user-defined types by OID identity and keep the enum constants that binding coerces from `unknown` literals by label identity, as `PostgreSQL` stores type and label OIDs in analyzed expressions.
    fn bind_stored_scalar_types(
        &mut self,
        engine: &dyn RoutineResolution,
        expression: &mut ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        super::stored_types::bind_scalar_type_identities(expression, &mut |name| {
            engine.resolve_type_name(name)
        })?;
        let resolver = self.query_function_type_resolver_for_subqueries(
            engine,
            std::slice::from_ref(expression),
            schema,
            subqueries,
            params,
        )?;
        // The coercions binding adds to an operator's operands, the constants it reads from `unknown` literals and the relabels of `oid` alias operands, are stored, as `PostgreSQL` stores them in an analyzed expression.
        crate::type_resolution::store_operand_coercions(expression, schema, params, &resolver)?;
        if !crate::type_resolution::contains_unknown_literal(expression) {
            return Ok(());
        }
        crate::type_resolution::fold_stored_enum_constants(expression, schema, params, &resolver)
            .map(drop)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "keeps execution context inputs aligned"
    )]
    fn bind_scalar_function_for_storage(
        &mut self,
        engine: &dyn RoutineResolution,
        name: &str,
        binding: &mut Option<FunctionBinding>,
        args: &[ScalarExpr],
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        let resolver = self.query_function_type_resolver_for_subqueries(
            engine, args, schema, subqueries, params,
        )?;
        let (argument_names, argument_types, explicit_variadic) =
            crate::function_call_argument_signature(args, schema, params, Some(&resolver))?;
        let selected = if crate::is_fixed_builtin(name) {
            crate::resolve_fixed_builtin_call(
                name,
                binding.as_ref(),
                &argument_names,
                &argument_types,
                explicit_variadic,
                Some(&resolver),
            )?
            .map(|resolved| resolved.selected)
        } else if let Some(array) = crate::type_resolution::resolve_array_transform_call(
            name,
            binding.as_ref(),
            args,
            &argument_types,
            explicit_variadic,
            &resolver,
        )? {
            array.overload
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

pub fn bind_query_plan_routines_for_storage(
    engine: &dyn RoutineResolution,
    plan: &mut QueryPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_analysis(ctes)?.bind_query_routines_for_storage(engine, plan, params, outer)
}

/// Bind a copy of a query lowered from stored syntax, keeping its shape so every bound identity can be carried back to that syntax.
pub fn bind_syntax_query_plan_routines(
    engine: &dyn RoutineResolution,
    plan: &mut QueryPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    let mut scope = SchemaScope::for_analysis(ctes)?;
    scope.preserve_syntax_shape = true;
    scope.bind_query_routines_for_storage(engine, plan, params, outer)
}

/// Bind every routine call owned by a stored scalar expression and validate its complete query-valued descendants against the expression's row scope. The plan keeps the shape of the syntax it was lowered from.
pub fn bind_expression_plan_routines_for_storage(
    engine: &dyn RoutineResolution,
    plan: &mut ExpressionPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    schema: &RowSchema,
) -> Result<Option<ColumnType>, SQLError> {
    let mut scope = SchemaScope::for_analysis(ctes)?;
    scope.preserve_syntax_shape = true;
    scope.stored_expression_outer = Some(schema.clone());
    for subquery in &mut plan.subqueries {
        scope.bind_query_routines_for_storage(engine, subquery, params, Some(schema))?;
    }
    scope.bind_scalar_routines_for_storage(
        engine,
        &mut plan.scalar,
        schema,
        &plan.subqueries,
        params,
    )?;
    scope.bind_expression_type(engine, &plan.scalar, schema, &plan.subqueries, params)
}
