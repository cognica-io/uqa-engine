//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent table-function identity binding for catalog-owned query plans.

use super::{
    cte_references_own_name, ordered_plan_ctes, overlay_outer_schema, rename_schema, CteScope,
    Engine, QueryPlan, RelationalPlan, RowSchema, SQLError, SQLParam, SchemaScope, SourcePlan,
};

impl SchemaScope {
    fn bind_query_table_functions_for_storage(
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
            let provisional = if self_recursive {
                self.bind_recursive_seed(engine, &plan.ctes[position].query, params, outer)?
            } else {
                self.bind_query_table_functions_for_storage(
                    engine,
                    &mut plan.ctes[position].query,
                    params,
                    outer,
                )?
            };
            let columns = plan.ctes[position].columns.clone();
            previous.push((
                name.clone(),
                self.ctes
                    .insert(name.clone(), rename_schema(&provisional, &columns, None)),
            ));
            if self_recursive {
                let complete = self.bind_query_table_functions_for_storage(
                    engine,
                    &mut plan.ctes[position].query,
                    params,
                    outer,
                )?;
                self.ctes
                    .insert(name, rename_schema(&complete, &columns, None));
            }
        }

        let result =
            self.bind_root_table_functions_for_storage(engine, &mut plan.root, params, outer);
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

    fn bind_root_table_functions_for_storage(
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
                    self.bind_nested_source_queries_for_storage(
                        engine,
                        source,
                        &block.subqueries,
                        params,
                        outer,
                    )?;
                }
                let expression_schema = overlay_outer_schema(&source_schema, outer);
                for subquery in &mut block.subqueries {
                    self.bind_query_table_functions_for_storage(
                        engine,
                        subquery,
                        params,
                        Some(&expression_schema),
                    )?;
                }
                self.bind_query_block(engine, block, params, outer, false)
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.bind_query_table_functions_for_storage(engine, left, params, outer)?;
                self.bind_query_table_functions_for_storage(engine, right, params, outer)?;
                for subquery in subqueries {
                    self.bind_query_table_functions_for_storage(engine, subquery, params, outer)?;
                }
                self.bind_root(engine, root, params, outer, false)
            }
            RelationalPlan::Values { subqueries, .. } => {
                for subquery in subqueries {
                    self.bind_query_table_functions_for_storage(engine, subquery, params, outer)?;
                }
                self.bind_root(engine, root, params, outer, false)
            }
        }
    }

    fn bind_nested_source_queries_for_storage(
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
                lateral,
                ..
            } => {
                self.bind_nested_source_queries_for_storage(
                    engine, left, subqueries, params, outer,
                )?;
                let left_schema = self.bind_source(engine, left, subqueries, params, outer)?;
                let implicit_lateral_function =
                    matches!(right.as_ref(), SourcePlan::Function { .. });
                let right_scope = (*lateral || implicit_lateral_function)
                    .then(|| overlay_outer_schema(&left_schema, outer));
                self.bind_nested_source_queries_for_storage(
                    engine,
                    right,
                    subqueries,
                    params,
                    right_scope.as_ref().or(outer),
                )
            }
            SourcePlan::Subquery { body, .. } => self
                .bind_query_table_functions_for_storage(engine, body, params, outer)
                .map(|_| ()),
            SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => {
                Ok(())
            }
        }
    }
}

pub(in crate::sql) fn bind_query_plan_table_functions_for_storage(
    engine: &Engine,
    plan: &mut QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_analysis(ctes)
        .bind_query_table_functions_for_storage(engine, plan, params, outer)
}
