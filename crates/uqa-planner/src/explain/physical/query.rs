//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Follow reachable statement children and lexical CTE scopes without materializing them.

use super::{visible_cte, Collector, DeferredCte, Scope};
use crate::filter_pushdown::{
    cte_output_filters_with_control, push_output_filter_into_query_plan_with_control,
    qualifier_filters_for_stmt_with_control,
};
use uqa_sql::{
    catalog::resolution::RelationLookupMode,
    plan::{CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, UnifiedPlan},
    semantics::{
        cte_references_own_name_with_control, cte_strategy::schedule_plan_ctes_with_control,
        order_cte_plans_with_control, query_plan_output_columns_with_control,
    },
    SQLError,
};

impl Collector<'_, '_, '_> {
    fn resolution(
        &self,
        bound: bool,
    ) -> Result<
        uqa_core::memory::Produced<uqa_sql::catalog::resolution::RelationNameResolution>,
        SQLError,
    > {
        let input = self.context.filters.correlation.resolution;
        uqa_sql::catalog::resolution::RelationNameResolution::from_inputs_with_control(
            input.search_path.iter().map(String::as_str),
            self.control.copy_text(&input.temporary_schema)?,
            input.temporary_namespace_allocated,
            &input.current_user,
            if bound {
                RelationLookupMode::Bound
            } else {
                RelationLookupMode::Dynamic
            },
            &self.control,
        )
    }

    pub(super) fn statement(
        &mut self,
        plan: &UnifiedPlan,
        scope: &Scope<'_>,
        path: &str,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        match plan {
            UnifiedPlan::Query(query) => self.query(query, scope, path),
            UnifiedPlan::Command(command) => self.command(command, scope, path),
        }
    }

    fn command(
        &mut self,
        command: &CommandPlan,
        outer: &Scope<'_>,
        path: &str,
    ) -> Result<(), SQLError> {
        if let CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } = command
        {
            return self.query(
                query,
                outer,
                &self.control.format(format_args!("{path}/Input"))?,
            );
        }
        let mut scope = outer.clone();
        let ordered = order_cte_plans_with_control(command.ctes(), self.control)?;
        for cte in ordered.iter().copied() {
            if cte.recursive {
                self.control.check()?;
                scope.try_insert(cte.name.as_str(), None)?;
            }
            self.cte_body(
                &cte.body,
                &scope,
                &self
                    .control
                    .format(format_args!("{path}/CTE {}", cte.name))?,
            )?;
            self.control.check()?;
            scope.try_insert(cte.name.as_str(), None)?;
        }
        for (index, query) in command.query_inputs().enumerate() {
            self.query(
                query,
                &scope,
                &self.control.format(format_args!("{path}/Input {index}"))?,
            )?;
        }
        if let Some(source) = command.source_input() {
            self.source(source, None, &scope, path)?;
        }
        self.mutation(command, &scope, path)?;
        if let CommandPlan::Explain { body, .. } = command {
            self.statement(body, &scope, path)?;
        }
        Ok(())
    }

    fn mutation(
        &mut self,
        command: &CommandPlan,
        scope: &Scope<'_>,
        path: &str,
    ) -> Result<(), SQLError> {
        let (table, qualifier, descendants, bound, predicate) = match command {
            CommandPlan::Update(plan) if plan.source.is_none() && plan.subqueries.is_empty() => (
                &plan.table,
                &plan.target_qualifier,
                plan.include_descendants,
                plan.target_relation_bound,
                plan.predicate.as_ref(),
            ),
            CommandPlan::Delete(plan) if plan.source.is_none() && plan.subqueries.is_empty() => (
                &plan.table,
                &plan.target_qualifier,
                plan.include_descendants,
                plan.target_relation_bound,
                plan.predicate.as_ref(),
            ),
            _ => return Ok(()),
        };
        let Some(predicate) = predicate else {
            return Ok(());
        };
        if scope.iter().any(|(_, value)| value.is_none())
            || uqa_sql::semantics::volatility::expr_contains_volatile_function(
                self.context.filters.volatility,
                predicate,
            )
        {
            return Ok(());
        }
        let correlation = self.context.filters.correlation;
        let resolution = self.resolution(bound)?;
        let Some(canonical) = correlation
            .catalog
            .table_name_resolved(&resolution, table)?
        else {
            return Ok(());
        };
        let tables = if descendants {
            self.context.statistics.hierarchy_scan_tables(&canonical)?
        } else {
            vec![canonical]
        };
        for table in tables {
            self.vector_predicate(&table, qualifier, predicate, path)?;
        }
        Ok(())
    }

    fn cte_body(
        &mut self,
        body: &CtePlanBody,
        scope: &Scope<'_>,
        path: &str,
    ) -> Result<(), SQLError> {
        match body {
            CtePlanBody::Query(query) => self.query(query, scope, path),
            CtePlanBody::Command(command) => self.command(command, scope, path),
        }
    }

    pub(super) fn query(
        &mut self,
        plan: &QueryPlan,
        outer: &Scope<'_>,
        path: &str,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        let resolution = self.resolution(plan.relations_bound)?;
        let mut context = self.context;
        context.filters.correlation.resolution = &resolution;
        let mut nested = Collector {
            context,
            params: self.params,
            control: self.control,
            scope_memory: self.scope_memory,
            costs: self.costs,
            output: &mut *self.output,
        };
        nested.query_root(plan, outer, path)?;
        Ok(())
    }

    fn query_root(
        &mut self,
        plan: &QueryPlan,
        outer: &Scope<'_>,
        path: &str,
    ) -> Result<(), SQLError> {
        let mut scope = outer.clone();
        let output_filters = cte_output_filters_with_control(
            self.context.filters,
            plan,
            self.filter_scope(&|name| {
                visible_cte(&scope, name)
                    || uqa_sql::semantics::cte_reference_name(name)
                        .is_some_and(|name| plan.ctes.iter().any(|cte| cte.name == name))
            }),
            &self.control,
        )?;
        let scheduled =
            schedule_plan_ctes_with_control(self.context.filters.volatility, plan, self.control)?;
        for scheduled in scheduled.iter() {
            let cte = scheduled.plan;
            if scheduled.deferred {
                let definition = DeferredCte {
                    plan: cte,
                    outer: scope.clone(),
                };
                self.control.check()?;
                scope.try_insert(cte.name.as_str(), Some(definition))?;
            } else {
                if cte.recursive {
                    self.control.check()?;
                    scope.try_insert(cte.name.as_str(), None)?;
                }
                let specialized = self.recursive_query(cte, output_filters.get(&cte.name))?;
                let path = self
                    .control
                    .format(format_args!("{path}/CTE {}", cte.name))?;
                if let Some(query) = specialized {
                    self.query(&query, &scope, &path)?;
                } else {
                    self.cte_body(&cte.body, &scope, &path)?;
                }
                self.control.check()?;
                scope.try_insert(cte.name.as_str(), None)?;
            }
        }
        let subqueries = match &plan.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(from) = &block.from {
                    let filters = qualifier_filters_for_stmt_with_control(
                        self.context.filters,
                        block,
                        from,
                        self.filter_scope(&|name| visible_cte(&scope, name)),
                        &self.control,
                    )?;
                    self.source(from, filters.as_ref(), &scope, path)?;
                }
                &block.subqueries
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.query(
                    left,
                    &scope,
                    &self.control.format(format_args!("{path}/Left"))?,
                )?;
                self.query(
                    right,
                    &scope,
                    &self.control.format(format_args!("{path}/Right"))?,
                )?;
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for (index, subquery) in subqueries.iter().enumerate() {
            self.query(
                subquery,
                &scope,
                &self
                    .control
                    .format(format_args!("{path}/Subquery {index}"))?,
            )?;
        }
        Ok(())
    }

    fn recursive_query(
        &self,
        cte: &CtePlan,
        filter: Option<&uqa_sql::plan::source_projection::CteOutputFilter>,
    ) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
        if !cte_references_own_name_with_control(cte, self.control)?
            || cte.search.is_some()
            || cte.cycle.is_some()
        {
            return Ok(None);
        }
        let (Some(query), Some((qualifier, filter))) = (cte.body.query(), filter) else {
            return Ok(None);
        };
        let RelationalPlan::SetOp { left, right, .. } = &query.root else {
            return Ok(None);
        };
        let inferred;
        let columns = if cte.columns.is_empty() {
            inferred = query_plan_output_columns_with_control(left, &self.control)?;
            let Some(columns) = &inferred else {
                return Ok(None);
            };
            columns.as_slice()
        } else {
            cte.columns.as_slice()
        };
        let anchor = push_output_filter_into_query_plan_with_control(
            self.context.filters,
            left,
            qualifier,
            filter,
            Some(columns),
            &self.control,
        )?;
        let step = push_output_filter_into_query_plan_with_control(
            self.context.filters,
            right,
            qualifier,
            filter,
            Some(columns),
            &self.control,
        )?;
        let (Some(anchor), Some(step)) = (anchor, step) else {
            return Ok(None);
        };
        query
            .copy_with_set_inputs(anchor, step, &self.control)
            .map(Some)
    }
}
