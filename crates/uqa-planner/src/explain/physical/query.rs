//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Follow reachable statement children and lexical CTE scopes without materializing them.

use super::{visible_cte, Collector, DeferredCte, Rc, Scope};
use crate::filter_pushdown::{
    cte_output_filters, push_output_filter_into_query_plan, qualifier_filters_for_stmt,
};
use uqa_sql::{
    catalog::resolution::RelationLookupMode,
    plan::{CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, UnifiedPlan},
    semantics::{
        cte_references_own_name, cte_strategy::schedule_plan_ctes, ordered_cte_plans,
        query_plan_output_columns,
    },
    SQLError, ScalarExpr,
};

impl Collector<'_> {
    pub(super) fn statement(
        &mut self,
        plan: &UnifiedPlan,
        scope: &Scope,
        path: &str,
    ) -> Result<(), SQLError> {
        match plan {
            UnifiedPlan::Query(query) => self.query(query, scope, path),
            UnifiedPlan::Command(command) => self.command(command, scope, path),
        }
    }

    fn command(
        &mut self,
        command: &CommandPlan,
        outer: &Scope,
        path: &str,
    ) -> Result<(), SQLError> {
        if let CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } = command
        {
            return self.query(query, outer, &format!("{path}/Input"));
        }
        let mut scope = outer.clone();
        for cte in ordered_cte_plans(command.ctes())? {
            if cte.recursive {
                scope.insert(cte.name.clone(), None);
            }
            self.cte_body(&cte.body, &scope, &format!("{path}/CTE {}", cte.name))?;
            scope.insert(cte.name.clone(), None);
        }
        for (index, query) in command.query_inputs().iter().enumerate() {
            self.query(query, &scope, &format!("{path}/Input {index}"))?;
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
        scope: &Scope,
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
        if scope.values().any(Option::is_none)
            || uqa_sql::semantics::volatility::expr_contains_volatile_function(
                self.context.filters.volatility,
                predicate,
            )
        {
            return Ok(());
        }
        let correlation = self.context.filters.correlation;
        let mut resolution = correlation.resolution.clone();
        resolution.lookup_mode = if bound {
            RelationLookupMode::Bound
        } else {
            RelationLookupMode::Dynamic
        };
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

    fn cte_body(&mut self, body: &CtePlanBody, scope: &Scope, path: &str) -> Result<(), SQLError> {
        match body {
            CtePlanBody::Query(query) => self.query(query, scope, path),
            CtePlanBody::Command(command) => self.command(command, scope, path),
        }
    }

    pub(super) fn query(
        &mut self,
        plan: &QueryPlan,
        outer: &Scope,
        path: &str,
    ) -> Result<(), SQLError> {
        let mut resolution = self.context.filters.correlation.resolution.clone();
        resolution.lookup_mode = if plan.relations_bound {
            RelationLookupMode::Bound
        } else {
            RelationLookupMode::Dynamic
        };
        let mut context = self.context;
        context.filters.correlation.resolution = &resolution;
        let mut nested = Collector {
            context,
            params: self.params,
            output: uqa_sql::result::ExplainPhysicalPlan::default(),
        };
        nested.query_root(plan, outer, path)?;
        self.output.nodes.extend(nested.output.nodes);
        Ok(())
    }

    fn query_root(&mut self, plan: &QueryPlan, outer: &Scope, path: &str) -> Result<(), SQLError> {
        let mut scope = outer.clone();
        let output_filters = cte_output_filters(
            self.context.filters,
            plan,
            self.filter_scope(&|name| {
                visible_cte(&scope, name)
                    || uqa_sql::semantics::cte_reference_name(name)
                        .is_some_and(|name| plan.ctes.iter().any(|cte| cte.name == name))
            }),
        )?;
        for scheduled in schedule_plan_ctes(self.context.filters.volatility, plan)? {
            let cte = scheduled.plan;
            if scheduled.deferred {
                let definition = DeferredCte {
                    plan: cte.clone(),
                    outer: Rc::new(scope.clone()),
                };
                scope.insert(cte.name.clone(), Some(definition));
            } else {
                if cte.recursive {
                    scope.insert(cte.name.clone(), None);
                }
                let specialized = self.recursive_query(cte, output_filters.get(&cte.name))?;
                let path = format!("{path}/CTE {}", cte.name);
                if let Some(query) = specialized {
                    self.query(&query, &scope, &path)?;
                } else {
                    self.cte_body(&cte.body, &scope, &path)?;
                }
                scope.insert(cte.name.clone(), None);
            }
        }
        match &plan.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(from) = &block.from {
                    let filters = qualifier_filters_for_stmt(
                        self.context.filters,
                        block,
                        from,
                        self.filter_scope(&|name| visible_cte(&scope, name)),
                    )?;
                    self.source(from, filters.as_ref(), &scope, path)?;
                }
                for (index, subquery) in block.subqueries.iter().enumerate() {
                    self.query(subquery, &scope, &format!("{path}/Subquery {index}"))?;
                }
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.query(left, &scope, &format!("{path}/Left"))?;
                self.query(right, &scope, &format!("{path}/Right"))?;
                for (index, subquery) in subqueries.iter().enumerate() {
                    self.query(subquery, &scope, &format!("{path}/Subquery {index}"))?;
                }
            }
            RelationalPlan::Values { subqueries, .. } => {
                for (index, subquery) in subqueries.iter().enumerate() {
                    self.query(subquery, &scope, &format!("{path}/Subquery {index}"))?;
                }
            }
        }
        Ok(())
    }

    fn recursive_query(
        &self,
        cte: &CtePlan,
        filter: Option<&(String, ScalarExpr)>,
    ) -> Result<Option<QueryPlan>, SQLError> {
        if !cte_references_own_name(cte) || cte.search.is_some() || cte.cycle.is_some() {
            return Ok(None);
        }
        let (Some(query), Some((qualifier, filter))) = (cte.body.query(), filter) else {
            return Ok(None);
        };
        let RelationalPlan::SetOp { left, right, .. } = &query.root else {
            return Ok(None);
        };
        let columns = if cte.columns.is_empty() {
            query_plan_output_columns(left)
        } else {
            Some(cte.columns.clone())
        };
        let Some(columns) = columns else {
            return Ok(None);
        };
        let anchor = push_output_filter_into_query_plan(
            self.context.filters,
            left,
            qualifier,
            filter,
            Some(&columns),
        )?;
        let step = push_output_filter_into_query_plan(
            self.context.filters,
            right,
            qualifier,
            filter,
            Some(&columns),
        )?;
        let (Some(anchor), Some(step)) = (anchor, step) else {
            return Ok(None);
        };
        let mut specialized = query.clone();
        if let RelationalPlan::SetOp { left, right, .. } = &mut specialized.root {
            **left = anchor;
            **right = step;
        }
        Ok(Some(specialized))
    }
}
