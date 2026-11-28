//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Visit statements in lexical order and rebuild eligible sources under their existing owner.

use super::{
    collect_conjuncts,
    materialization::{EmbeddedPredicates, PredicateOutput, PreparedJoin},
    output::{CallerPredicates, Output},
    CommandPlan, OptimizerResult, RelationalPlan, SourcePlan, SourceStatistics,
};
use crate::optimizer::OptimizerError;
use uqa_core::memory::{ProductionControl, ProductionVec};
use uqa_sql::{
    plan::{
        CommandQuerySlot, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
        MutableSourcePlan, OwnedExecutablePlan, QueryChildSlot, SourceChildSlot,
    },
    SQLError, ScalarExpr,
};

pub(in crate::optimizer) fn reorder_caller_plan(
    plan: MutableExecutablePlan,
    statistics: &dyn SourceStatistics,
    work: &ProductionControl<'_>,
) -> OptimizerResult<OwnedExecutablePlan> {
    let storage = ProductionControl::uncontrolled();
    let mut output = Output::new(work);
    let plan = reorder(
        plan,
        statistics,
        work,
        &storage,
        &mut CallerPredicates::new(&mut output),
    )?;
    let (plan, _) = plan.finish(&storage)?.into_parts();
    Ok(OwnedExecutablePlan::from_parts(plan, output.finish()))
}

pub(in crate::optimizer) fn reorder_owned_plan(
    plan: MutableExecutablePlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<MutableExecutablePlan> {
    reorder(plan, statistics, control, control, &mut EmbeddedPredicates)
        .map_err(|error| OptimizerError::Expression(error.into_sql()))
}

pub(super) fn reorder(
    plan: MutableExecutablePlan,
    statistics: &dyn SourceStatistics,
    work: &ProductionControl<'_>,
    storage: &ProductionControl<'_>,
    output: &mut impl PredicateOutput,
) -> OptimizerResult<MutableExecutablePlan> {
    Context {
        statistics,
        work,
        storage,
        output,
    }
    .plan(plan)
}

pub(super) fn where_predicates<'q, 'work>(
    source: &SourcePlan,
    predicate: Option<&'q ScalarExpr>,
    work: ProductionControl<'work>,
) -> Result<ProductionVec<'work, &'q ScalarExpr>, SQLError> {
    let mut predicates = ProductionVec::new(work);
    if matches!(source, SourcePlan::Join { .. }) {
        if let Some(predicate) = predicate {
            collect_conjuncts(predicate, &mut predicates, &work)?;
        }
    }
    Ok(predicates)
}

struct Context<'a, 'c, P> {
    statistics: &'a dyn SourceStatistics,
    work: &'a ProductionControl<'c>,
    storage: &'a ProductionControl<'c>,
    output: &'a mut P,
}
impl<P: PredicateOutput> Context<'_, '_, P> {
    fn plan(&mut self, plan: MutableExecutablePlan) -> OptimizerResult<MutableExecutablePlan> {
        self.work.check().map_err(SQLError::from)?;
        Ok(match plan {
            MutableExecutablePlan::Query(query) => MutableExecutablePlan::Query(self.query(query)?),
            MutableExecutablePlan::Command(command) => {
                MutableExecutablePlan::Command(self.command(command)?)
            }
        })
    }
    fn query(&mut self, query: MutableQueryPlan) -> OptimizerResult<MutableQueryPlan> {
        let local = super::scope::JoinStatistics::new(self.statistics, &query.ctes, self.work)?;
        Context {
            statistics: local.selected(),
            work: self.work,
            storage: self.storage,
            output: self.output,
        }
        .query_in_scope(query)
    }
    fn query_in_scope(&mut self, mut query: MutableQueryPlan) -> OptimizerResult<MutableQueryPlan> {
        self.work.check().map_err(SQLError::from)?;
        for index in 0..query.ctes.len() {
            query = query.try_map_cte(index, self.storage, |body| self.plan(body))?;
        }
        let subqueries = match &query.root {
            RelationalPlan::QueryBlock(block) => block.subqueries.len(),
            RelationalPlan::Values { subqueries, .. } => subqueries.len(),
            RelationalPlan::SetOp { .. } => {
                query = query.try_map_query_child(QueryChildSlot::Left, self.storage, |query| {
                    self.query(query)
                })?;
                query =
                    query.try_map_query_child(QueryChildSlot::Right, self.storage, |query| {
                        self.query(query)
                    })?;
                return Ok(query);
            }
        };
        if matches!(&query.root, RelationalPlan::QueryBlock(_)) {
            let (parent, source) = query.take_source(self.storage)?;
            query = parent;
            if let Some(source) = source {
                let RelationalPlan::QueryBlock(block) = &query.root else {
                    unreachable!()
                };
                let predicates = where_predicates(&source, block.r#where.as_ref(), *self.work)?;
                let source = self.source(source, &predicates)?;
                drop(predicates);
                query = query.replace_source(Some(source), self.storage)?;
            }
        }
        for index in 0..subqueries {
            query = query.try_map_query_child(
                QueryChildSlot::Subquery(index),
                self.storage,
                |query| self.query(query),
            )?;
        }
        Ok(query)
    }
    fn command(&mut self, command: MutableCommandPlan) -> OptimizerResult<MutableCommandPlan> {
        let local = super::scope::JoinStatistics::new(self.statistics, command.ctes(), self.work)?;
        Context {
            statistics: local.selected(),
            work: self.work,
            storage: self.storage,
            output: self.output,
        }
        .command_in_scope(command)
    }
    fn command_in_scope(
        &mut self,
        mut command: MutableCommandPlan,
    ) -> OptimizerResult<MutableCommandPlan> {
        self.work.check().map_err(SQLError::from)?;
        match &*command {
            CommandPlan::DeclareCursor { .. } => {
                return command.try_map_query_child_with_parent(
                    CommandQuerySlot::Body,
                    self.storage,
                    |_, query| self.query(query),
                )
            }
            CommandPlan::Call { args, .. } => {
                let count = args.len();
                for argument in 0..count {
                    let CommandPlan::Call { args, .. } = &*command else {
                        unreachable!()
                    };
                    for query in 0..args[argument].subqueries.len() {
                        command = command.try_map_query_child_with_parent(
                            CommandQuerySlot::CallArgumentSubquery { argument, query },
                            self.storage,
                            |_, query| self.query(query),
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
            command = command.try_map_cte(index, self.storage, |body| self.plan(body))?;
        }
        let subqueries = match &*command {
            CommandPlan::Insert(plan) => {
                let count = plan.subqueries.len();
                if plan.source.is_some() {
                    command = command.try_map_query_child_with_parent(
                        CommandQuerySlot::InsertSource,
                        self.storage,
                        |_, query| self.query(query),
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
            command = command
                .try_map_source_with_parent(self.storage, |_, source| self.source(source, &[]))?;
        }
        for index in 0..subqueries {
            command = command.try_map_query_child_with_parent(
                CommandQuerySlot::Subquery(index),
                self.storage,
                |_, query| self.query(query),
            )?;
        }
        Ok(command)
    }
    fn source(
        &mut self,
        mut source: MutableSourcePlan,
        external: &[&ScalarExpr],
    ) -> OptimizerResult<MutableSourcePlan> {
        self.work.check().map_err(SQLError::from)?;
        match &*source {
            SourcePlan::Join { .. } => {
                source = source.try_map_child(SourceChildSlot::Left, self.storage, |source| {
                    self.source(source, &[])
                })?;
                source = source.try_map_child(SourceChildSlot::Right, self.storage, |source| {
                    self.source(source, &[])
                })?;
            }
            SourcePlan::Subquery { .. } => {
                source = source.try_map_query(self.storage, |query| self.query(query))?;
            }
            SourcePlan::Table { .. }
            | SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::FunctionGroup { .. }
            | SourcePlan::RetainedRows(_) => {}
        }
        let Some(prepared) =
            PreparedJoin::select(&source, external, self.statistics, self.work, self.output)?
        else {
            return Ok(source);
        };
        Ok(prepared.assemble(source, self.output, self.storage)?)
    }
}
