//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native join rewrites keep independently owned scalar leaves and reuse the original source boxes throughout topology changes.

use super::{
    collect_conjuncts, selection::select_graph, CommandPlan, OptimizerResult, RelationalPlan,
    SourcePlan, SourceStatistics,
};
use uqa_core::memory::{ProductionControl, ProductionVec};
use uqa_sql::{
    plan::{
        CommandQuerySlot, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
        MutableSourcePlan, OwnedScalarExpr, QueryChildSlot, SourceChildSlot,
    },
    SQLError, ScalarExpr,
};

mod materialization;

pub(in crate::optimizer) fn reorder_owned_plan(
    plan: MutableExecutablePlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<MutableExecutablePlan> {
    Context {
        statistics,
        control,
    }
    .plan(plan)
    .map_err(Into::into)
}

struct Context<'a, 'c> {
    statistics: &'a dyn SourceStatistics,
    control: &'a ProductionControl<'c>,
}
impl Context<'_, '_> {
    fn plan(&self, plan: MutableExecutablePlan) -> Result<MutableExecutablePlan, SQLError> {
        self.control.check()?;
        Ok(match plan {
            MutableExecutablePlan::Query(query) => MutableExecutablePlan::Query(self.query(query)?),
            MutableExecutablePlan::Command(command) => {
                MutableExecutablePlan::Command(self.command(command)?)
            }
        })
    }
    fn query(&self, mut query: MutableQueryPlan) -> Result<MutableQueryPlan, SQLError> {
        self.control.check()?;
        for index in 0..query.ctes.len() {
            query = query.map_cte(index, self.control, |body| self.plan(body))?;
        }
        let subqueries = match &query.root {
            RelationalPlan::QueryBlock(block) => block.subqueries.len(),
            RelationalPlan::Values { subqueries, .. } => subqueries.len(),
            RelationalPlan::SetOp { .. } => {
                query = query.map_query_child(QueryChildSlot::Left, self.control, |query| {
                    self.query(query)
                })?;
                query = query.map_query_child(QueryChildSlot::Right, self.control, |query| {
                    self.query(query)
                })?;
                return Ok(query);
            }
        };
        if matches!(&query.root, RelationalPlan::QueryBlock(_)) {
            let (parent, source) = query.take_source(self.control)?;
            query = parent;
            if let Some(source) = source {
                let mut predicates = ProductionVec::new(*self.control);
                if matches!(&*source, SourcePlan::Join { .. }) {
                    let RelationalPlan::QueryBlock(block) = &query.root else {
                        unreachable!()
                    };
                    if let Some(predicate) = &block.r#where {
                        collect_conjuncts(predicate, &mut predicates, self.control)?;
                    }
                }
                let source = self.source(source, &predicates)?;
                drop(predicates);
                query = query.replace_source(Some(source), self.control)?;
            }
        }
        for index in 0..subqueries {
            query =
                query.map_query_child(QueryChildSlot::Subquery(index), self.control, |query| {
                    self.query(query)
                })?;
        }
        Ok(query)
    }
    fn command(&self, mut command: MutableCommandPlan) -> Result<MutableCommandPlan, SQLError> {
        self.control.check()?;
        match &*command {
            CommandPlan::DeclareCursor { .. } => {
                return command.map_query_body(self.control, |query| self.query(query))
            }
            CommandPlan::Call { args, .. } => {
                let count = args.len();
                for argument in 0..count {
                    let CommandPlan::Call { args, .. } = &*command else {
                        unreachable!()
                    };
                    for query in 0..args[argument].subqueries.len() {
                        command = command.map_query_child(
                            CommandQuerySlot::CallArgumentSubquery { argument, query },
                            self.control,
                            |query| self.query(query),
                        )?;
                    }
                }
                return Ok(command);
            }
            CommandPlan::Insert(_)
            | CommandPlan::Update(_)
            | CommandPlan::Delete(_)
            | CommandPlan::Merge(_) => {}
            _ => return Ok(command),
        }
        for index in 0..command.ctes().len() {
            command = command.map_cte(index, self.control, |body| self.plan(body))?;
        }
        let subqueries = match &*command {
            CommandPlan::Insert(plan) => {
                let count = plan.subqueries.len();
                if plan.source.is_some() {
                    command = command.map_query_child(
                        CommandQuerySlot::InsertSource,
                        self.control,
                        |query| self.query(query),
                    )?;
                }
                count
            }
            CommandPlan::Update(plan) => plan.subqueries.len(),
            CommandPlan::Delete(plan) => plan.subqueries.len(),
            CommandPlan::Merge(plan) => plan.subqueries.len(),
            _ => unreachable!("selected mutation command"),
        };
        if !matches!(&*command, CommandPlan::Insert(_)) {
            command = command.map_source(self.control, |source| self.source(source, &[]))?;
        }
        for index in 0..subqueries {
            command = command.map_query_child(
                CommandQuerySlot::Subquery(index),
                self.control,
                |query| self.query(query),
            )?;
        }
        Ok(command)
    }
    fn source(
        &self,
        mut source: MutableSourcePlan,
        external: &[&ScalarExpr],
    ) -> Result<MutableSourcePlan, SQLError> {
        self.control.check()?;
        match &*source {
            SourcePlan::Join { .. } => {
                source = source.map_child(SourceChildSlot::Left, self.control, |source| {
                    self.source(source, &[])
                })?;
                source = source.map_child(SourceChildSlot::Right, self.control, |source| {
                    self.source(source, &[])
                })?;
            }
            SourcePlan::Subquery { .. } => {
                source = source.map_query(self.control, |query| self.query(query))?;
            }
            SourcePlan::Table { .. }
            | SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::FunctionGroup { .. }
            | SourcePlan::RetainedRows(_) => {}
        }
        self.rewrite(source, external)
    }
    fn rewrite(
        &self,
        source: MutableSourcePlan,
        external: &[&ScalarExpr],
    ) -> Result<MutableSourcePlan, SQLError> {
        let Some(mut selected) = select_graph(&source, external, self.statistics, self.control)
            .map_err(|error| match error {
                super::super::OptimizerError::Expression(error) => error,
                super::super::OptimizerError::JoinGraph(error) => {
                    SQLError::Internal(format!("optimize SQL join order: {error}"))
                }
            })?
        else {
            return Ok(source);
        };
        let mut predicates = materialization::Predicates::new(selected.masks.len(), self.control)?;
        for (index, expression) in selected.inferred.iter().enumerate() {
            predicates.set(
                selected.original_count + index,
                expression.copy_for_rewrite(self.control)?,
            );
        }
        for mask in selected.masks.as_mut_slice() {
            *mask = super::bindings::destination(&selected.tree, *mask);
        }
        let source = materialization::reorder(
            source,
            &selected.tree,
            &selected.masks,
            &mut predicates,
            self.control,
        )?;
        assert!(
            predicates.all_consumed(),
            "selected join retains every original and inferred predicate"
        );
        Ok(source)
    }
}
