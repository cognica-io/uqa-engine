//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent exact routine identity binding for catalog-owned query plans.

use super::{
    extend_cte_generated_schema, extend_recursive_cte_binding_schema,
    operator_join_relation_schemas, overlay_outer_schema, rename_schema,
    stored_routines::StoredCallOrigin, BindingContext, ColumnType, QueryPlan, RelationalPlan,
    RowSchema, SQLError, SQLParam, ScalarExpr, SchemaScope, SourcePlan, SourceScope,
};
use crate::ast::FunctionBinding;
use crate::plan::ExpressionPlan;
use crate::routines::RoutineResolution;
use crate::{ColumnIdentity, FunctionTypeResolver};

mod scalars;

impl SchemaScope {
    pub(super) fn with_stored_outer_internal_aliases(
        &self,
        schema: &RowSchema,
    ) -> Result<RowSchema, SQLError> {
        match self.stored_expression_outer.as_ref() {
            Some(outer) if schema.physical_width() >= outer.physical_width() => {
                RowSchema::with_trailing_internal_aliases_with_control(
                    schema,
                    outer,
                    &self.control(),
                )
            }
            _ => {
                self.control().check()?;
                Ok(schema.clone())
            }
        }
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

    async fn bind_cte_routines_for_storage<W: crate::catalog::work::Work + Send>(
        &mut self,
        routines: &dyn RoutineResolution,
        body: &mut crate::plan::CtePlanBody,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<RowSchema, SQLError> {
        let crate::plan::CtePlanBody::Command(command) = body else {
            let owner = self.production.clone();
            return crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                owner.control(),
                self.bind_query_routines_for_storage_with_work(
                    routines,
                    body.query_mut().expect("query CTE body"),
                    params,
                    outer,
                    work,
                )
            );
        };
        let owner = self.production.clone();
        crate::catalog::work::nested!(
            W::MAY_SUSPEND,
            owner.control(),
            self.bind_command_routines_for_storage_with_work(
                routines, command, params, outer, work
            )
        )?;
        self.bind_command_returning(routines, command, params)
    }

    pub(super) async fn bind_cte_routine_schemas<W: crate::catalog::work::Work + Send>(
        &mut self,
        routines: &dyn RoutineResolution,
        ctes: &mut [crate::plan::CtePlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<super::ctes::CteScopeStack, SQLError> {
        let owner = self.production.clone();
        let control = owner.control();
        let positions = crate::semantics::cte_positions_with_work(ctes, control, work).await?;
        let mut previous = super::ctes::CteScopeStack::new(positions.len(), &control)?;
        for &position in positions.iter() {
            work.checkpoint().await?;
            let self_recursive =
                crate::semantics::cte_references_own_name_with_control(&ctes[position], control)?;
            if let Some(cycle) = ctes[position].cycle.as_mut() {
                let schema = RowSchema::default();
                self.bind_scalar_routines_for_storage_with_work(
                    routines,
                    &mut cycle.mark_value,
                    &schema,
                    &[],
                    params,
                    work,
                )
                .await?;
                self.bind_scalar_routines_for_storage_with_work(
                    routines,
                    &mut cycle.mark_default,
                    &schema,
                    &[],
                    params,
                    work,
                )
                .await?;
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
                    work,
                )
                .await?
            };
            let provisional = rename_schema(&provisional, &ctes[position].columns, None);
            let provisional = if self_recursive {
                extend_recursive_cte_binding_schema(routines, &ctes[position], provisional, params)?
            } else {
                extend_cte_generated_schema(routines, &ctes[position], provisional, params)?
            };
            previous.push(self.install_cte_schema(&ctes[position], Some(provisional))?);
            if self_recursive {
                let complete = self
                    .bind_cte_routines_for_storage(
                        routines,
                        &mut ctes[position].body,
                        params,
                        outer,
                        work,
                    )
                    .await?;
                let complete = rename_schema(&complete, &ctes[position].columns, None);
                let complete =
                    extend_cte_generated_schema(routines, &ctes[position], complete, params)?;
                self.set_cte_schema(&ctes[position].name, complete)?;
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
        crate::catalog::work::inline(self.bind_query_routines_for_storage_with_work(
            routines,
            plan,
            params,
            outer,
            &mut (),
        ))
    }

    pub(super) async fn bind_query_routines_for_storage_with_work<
        W: crate::catalog::work::Work + Send,
    >(
        &mut self,
        routines: &dyn RoutineResolution,
        plan: &mut QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<RowSchema, SQLError> {
        let previous = self
            .bind_cte_routine_schemas(routines, &mut plan.ctes, params, outer, work)
            .await?;

        let result = self
            .bind_root_routines_for_storage_with_work(routines, &mut plan.root, params, outer, work)
            .await;
        self.restore_cte_schemas(previous);
        result
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    async fn bind_root_routines_for_storage_with_work<W: crate::catalog::work::Work + Send>(
        &mut self,
        routines: &dyn RoutineResolution,
        root: &mut RelationalPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<RowSchema, SQLError> {
        match root {
            RelationalPlan::QueryBlock(block) => {
                // The sources' own expressions resolve before the sources are analyzed, so that an analysis sees the names a nested query resolved, such as its output names in GROUP BY.
                if let Some(source) = block.from.as_mut() {
                    self.bind_source_routines_for_storage_with_work(
                        routines,
                        source,
                        &block.subqueries,
                        params,
                        outer,
                        work,
                    )
                    .await?;
                }
                let source_schema = match block.from.as_mut() {
                    Some(source) => {
                        self.bind_source_for_execution_with_work(
                            routines,
                            source,
                            &block.subqueries,
                            params,
                            outer,
                            work,
                        )
                        .await?
                    }
                    None => RowSchema::default(),
                };
                let source_schema =
                    super::analysis::with_query_source_columns(&source_schema, block);
                let expression_schema = overlay_outer_schema(&source_schema, outer);
                for subquery in &mut block.subqueries {
                    crate::catalog::work::nested!(
                        W::MAY_SUSPEND,
                        self.production.control(),
                        self.bind_query_routines_for_storage_with_work(
                            routines,
                            subquery,
                            params,
                            Some(&expression_schema),
                            work
                        )
                    )?;
                }
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.production.control(),
                    self.bind_query_routines_for_storage_with_work(
                        routines, left, params, outer, work
                    )
                )?;
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.production.control(),
                    self.bind_query_routines_for_storage_with_work(
                        routines, right, params, outer, work
                    )
                )?;
                for subquery in subqueries {
                    crate::catalog::work::nested!(
                        W::MAY_SUSPEND,
                        self.production.control(),
                        self.bind_query_routines_for_storage_with_work(
                            routines, subquery, params, outer, work
                        )
                    )?;
                }
            }
            RelationalPlan::Values { subqueries, .. } => {
                for subquery in subqueries {
                    crate::catalog::work::nested!(
                        W::MAY_SUSPEND,
                        self.production.control(),
                        self.bind_query_routines_for_storage_with_work(
                            routines, subquery, params, outer, work
                        )
                    )?;
                }
            }
        }

        let set_output = match &*root {
            RelationalPlan::SetOp { .. } => Some(
                self.bind_root_with_work(routines, root, params, outer, work)
                    .await?,
            ),
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
                let source = match block.from.as_ref() {
                    Some(source) => {
                        self.bind_source_with_work(
                            routines,
                            source,
                            &block.subqueries,
                            params,
                            outer,
                            work,
                        )
                        .await?
                    }
                    None => RowSchema::default(),
                };
                let source_schema = super::analysis::with_query_source_columns(&source, block);
                let expression_schema = overlay_outer_schema(&source_schema, outer);
                if let Some(filter) = block.r#where.as_mut() {
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        filter,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        work,
                    )
                    .await?;
                }
                let labels = super::routine_parameters::column_labels(&block.projections);
                for projection in &mut block.projections {
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        &mut projection.expr,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        work,
                    )
                    .await?;
                }
                super::routine_parameters::keep_column_labels(&mut block.projections, labels);
                let expanded = crate::semantics::expand_bound_projection_stars_in_scope(
                    &block.projections,
                    &source_schema,
                    &expression_schema,
                )?;
                // Stored syntax keeps grouping names and stars as written. Only the grouping inputs and expanded projections participate in this validation.
                let (mut groups, mut sets);
                let (projections, groups, sets) = if self.preserve_syntax_shape {
                    groups = block.group_by.clone();
                    sets = block.grouping_sets.clone();
                    (&expanded, &mut groups, &mut sets)
                } else {
                    block.projections = expanded;
                    (
                        &block.projections,
                        &mut block.group_by,
                        &mut block.grouping_sets,
                    )
                };
                self.bind_grouping_variable_sites(
                    groups.iter_mut().chain(sets.iter_mut().flatten()),
                    projections,
                    &source_schema,
                );
                crate::semantics::grouping_sets::bind_grouping_items(
                    routines,
                    groups.iter_mut().chain(sets.iter_mut().flatten()),
                    projections,
                    &source_schema,
                    None,
                    params,
                )?;
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
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        expression,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        work,
                    )
                    .await?;
                }
                if let Some(having) = block.having.as_mut() {
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        having,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        work,
                    )
                    .await?;
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
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        &mut order.expr,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        work,
                    )
                    .await?;
                }
                if let Some(limit) = block.limit.as_mut() {
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        limit,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        work,
                    )
                    .await?;
                }
                if let Some(offset) = block.offset.as_mut() {
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        offset,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        work,
                    )
                    .await?;
                }
                for expression in &mut block.distinct_on {
                    if names_output(expression)
                        || self.output_takes_variable_site(expression, &output_names)
                    {
                        continue;
                    }
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        expression,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        work,
                    )
                    .await?;
                }
                for expression in block
                    .windows
                    .iter_mut()
                    .flat_map(|window| window.spec.expressions_mut())
                {
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        expression,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        work,
                    )
                    .await?;
                }
                // FROM and its scalar subqueries have their final bindings. A deferred view rewrite can change the source and its outer namespace before output analysis.
                if block.from.is_some() && block.deferred_outer_rewrites.is_empty() {
                    return self.bind_query_block_output(routines, block, source, params, outer);
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
                    self.bind_scalar_routines_for_storage_with_work(
                        routines,
                        &mut order.expr,
                        output,
                        subqueries,
                        params,
                        work,
                    )
                    .await?;
                }
                if let Some(limit) = limit {
                    self.bind_scalar_routines_for_storage_with_work(
                        routines, limit, output, subqueries, params, work,
                    )
                    .await?;
                }
                if let Some(offset) = offset {
                    self.bind_scalar_routines_for_storage_with_work(
                        routines, offset, output, subqueries, params, work,
                    )
                    .await?;
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                let input = outer.cloned().unwrap_or_default();
                for expression in rows.iter_mut().flatten() {
                    self.bind_scalar_routines_for_storage_with_work(
                        routines, expression, &input, subqueries, params, work,
                    )
                    .await?;
                }
            }
        }
        self.bind_root_with_work(routines, root, params, outer, work)
            .await
    }

    pub(super) async fn bind_source_routines_for_storage_with_work<
        W: crate::catalog::work::Work + Send,
    >(
        &mut self,
        engine: &dyn RoutineResolution,
        source: &mut SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.bind_source_routines_in_scope_with_work(
            engine,
            source,
            subqueries,
            params,
            SourceScope::new(outer),
            work,
        )
        .await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    pub(super) async fn bind_source_routines_in_scope_with_work<
        W: crate::catalog::work::Work + Send,
    >(
        &mut self,
        engine: &dyn RoutineResolution,
        source: &mut SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        scope: SourceScope<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let input = (!matches!(source, SourcePlan::Join { .. }))
            .then(|| scope.input(source))
            .flatten();
        let outer = input.as_ref();
        match source {
            crate::plan::SourcePlan::RetainedRows(_) => Ok(()),
            SourcePlan::Join {
                left,
                right,
                on,
                lateral,
                kind,
                ..
            } => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.production.control(),
                    self.bind_source_routines_in_scope_with_work(
                        engine,
                        left,
                        subqueries,
                        params,
                        scope.with_names(),
                        work
                    )
                )?;
                let left_schema = self
                    .bind_source_in_scope_with_work(
                        engine,
                        left,
                        subqueries,
                        params,
                        scope.with_names(),
                        work,
                    )
                    .await?;
                let preceding = scope.right_namespace(left, &left_schema, *kind);
                let right_scope = SourceScope {
                    outer: scope.outer,
                    preceding: Some(&preceding),
                    inherited_lateral: *lateral,
                    retain_names: true,
                };
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.production.control(),
                    self.bind_source_routines_in_scope_with_work(
                        engine,
                        right,
                        subqueries,
                        params,
                        right_scope,
                        work
                    )
                )?;
                if let Some(on) = on {
                    let right_schema = self
                        .bind_source_in_scope_with_work(
                            engine,
                            right,
                            subqueries,
                            params,
                            right_scope,
                            work,
                        )
                        .await?;
                    let input = RowSchema::join(&left_schema, &right_schema, std::iter::empty());
                    let input = super::with_query_table_pseudo_columns(&input);
                    let condition = scope.condition();
                    let input = overlay_outer_schema(&input, condition.as_ref());
                    self.bind_scalar_routines_for_storage_with_work(
                        engine, on, &input, subqueries, params, work,
                    )
                    .await?;
                }
                Ok(())
            }
            SourcePlan::Subquery { body, .. } => crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                self.production.control(),
                self.bind_query_routines_for_storage_with_work(engine, body, params, outer, work)
            )
            .map(|_| ()),
            SourcePlan::Values { rows, .. } => {
                let input = outer.cloned().unwrap_or_default();
                for expression in rows.iter_mut().flatten() {
                    self.bind_scalar_routines_for_storage_with_work(
                        engine, expression, &input, subqueries, params, work,
                    )
                    .await?;
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
                        self.bind_scalar_routines_for_storage_with_work(
                            engine, expression, input, subqueries, params, work,
                        )
                        .await?;
                    }
                    return Ok(());
                }
                let input = outer.cloned().unwrap_or_default();
                for expression in args {
                    self.bind_scalar_routines_for_storage_with_work(
                        engine, expression, &input, subqueries, params, work,
                    )
                    .await?;
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
                            self.bind_scalar_routines_for_storage_with_work(
                                engine, expression, input, subqueries, params, work,
                            )
                            .await?;
                        }
                        continue;
                    }
                    let input = outer.cloned().unwrap_or_default();
                    for expression in &mut function.args {
                        self.bind_scalar_routines_for_storage_with_work(
                            engine, expression, &input, subqueries, params, work,
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            SourcePlan::Table { .. } => Ok(()),
        }
    }
}

pub fn bind_query_plan_routines_for_storage(
    engine: &dyn RoutineResolution,
    plan: &mut QueryPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    StoredCallOrigin::Definition.bind_query(engine, plan, params, ctes, outer)
}

impl StoredCallOrigin {
    pub(crate) fn bind_query(
        self,
        engine: &dyn RoutineResolution,
        plan: &mut QueryPlan,
        params: &[SQLParam],
        ctes: &BindingContext,
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        crate::catalog::work::inline(self.bind_query_in(
            &mut SchemaScope::for_analysis(ctes)?,
            engine,
            plan,
            params,
            outer,
            &mut (),
        ))
    }
    pub(super) async fn bind_query_in<W: crate::catalog::work::Work + Send>(
        self,
        scope: &mut SchemaScope,
        routines: &dyn RoutineResolution,
        query: &mut QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<RowSchema, SQLError> {
        scope.stored_call_origin = self;
        scope
            .bind_query_routines_for_storage_with_work(routines, query, params, outer, work)
            .await
    }
    pub(crate) async fn bind_query_from(
        self,
        routines: &dyn RoutineResolution,
        query: &mut QueryPlan,
        params: &[SQLParam],
        binding: super::production::ControlledBindingContext<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<crate::plan::ExecutableMemory, SQLError> {
        let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
        self.bind_query_in(&mut scope, routines, query, params, None, input)
            .await?;
        Ok(scope.stored_binding_memory.into())
    }
}

impl SchemaScope {
    pub(super) fn store_call_selection(
        &self,
        name: &str,
        binding: &mut Option<FunctionBinding>,
        selected: FunctionBinding,
    ) {
        if !self
            .stored_call_origin
            .keeps_runtime_lookup(name, binding.as_ref(), &selected)
        {
            FunctionBinding::rebind(binding, selected);
        }
    }
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
    bind_stored_expression(engine, plan, params, ctes, schema, None)
}

pub(super) fn bind_rule_condition_routines(
    engine: &dyn RoutineResolution,
    plan: &mut ExpressionPlan,
    ctes: &BindingContext,
    schema: &RowSchema,
    event: crate::ast::RuleEvent,
    origin: StoredCallOrigin,
) -> Result<Option<ColumnType>, SQLError> {
    bind_stored_expression(engine, plan, &[], ctes, schema, Some((event, origin)))
}

fn bind_stored_expression(
    engine: &dyn RoutineResolution,
    plan: &mut ExpressionPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    schema: &RowSchema,
    rule: Option<(crate::ast::RuleEvent, StoredCallOrigin)>,
) -> Result<Option<ColumnType>, SQLError> {
    let mut scope = SchemaScope::for_analysis(ctes)?;
    if let Some((event, origin)) = rule {
        scope.rule_condition_event = Some(event);
        scope.stored_call_origin = origin;
    }
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
