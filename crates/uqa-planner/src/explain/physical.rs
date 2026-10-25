//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static physical diagnostics over the statement's retained catalog. No query operator is executed here.

use crate::{
    filter_pushdown::context::{FilterPushdownContext, FilterPushdownScope},
    retrieval_planning::RetrievalPlanningCatalog,
    statement_planning::PlannerStatisticsCatalog,
};
use uqa_core::memory::{
    BudgetedSharedMap, MemoryBudget, Produced, ProductionControl, ProductionVec,
};
use uqa_sql::{
    plan::{CtePlan, UnifiedPlan},
    result::{explain::ExplainValue, ExplainPhysicalPlan},
    retrieval::ControlledConstantEvaluator,
    SQLError, SQLParam,
};

mod metadata;
mod query;
mod source;
mod vector;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub struct PhysicalExplainContext<'a> {
    pub retrieval: &'a dyn RetrievalPlanningCatalog,
    pub statistics: &'a dyn PlannerStatisticsCatalog,
    pub filters: FilterPushdownContext<'a>,
    pub evaluate: &'a ControlledConstantEvaluator<'a>,
    /// Validate the retained provider metadata for a concrete source after CTE and view expansion.
    pub validate_source: &'a dyn Fn(
        &uqa_sql::catalog::resolution::RelationNameResolution,
        &str,
    ) -> Result<(), SQLError>,
}

struct DeferredCte<'a> {
    plan: &'a CtePlan,
    outer: Scope<'a>,
}
type Scope<'a> = BudgetedSharedMap<&'a str, Option<DeferredCte<'a>>>;

pub fn physical_plan(
    context: PhysicalExplainContext<'_>,
    body: &UnifiedPlan,
    params: &[SQLParam],
) -> Result<ExplainPhysicalPlan, SQLError> {
    Ok(physical_plan_with_control(
        context,
        body,
        params,
        true,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary physical explanation"))
}

pub fn physical_plan_with_control(
    context: PhysicalExplainContext<'_>,
    body: &UnifiedPlan,
    params: &[SQLParam],
    costs: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<ExplainPhysicalPlan>, SQLError> {
    control.check()?;
    let memory = control
        .budget()
        .cloned()
        .unwrap_or_else(|| MemoryBudget::new(usize::MAX));
    let mut output = ProductionVec::new(*control);
    let mut collector = Collector {
        context,
        params,
        control: *control,
        scope_memory: &memory,
        costs,
        output: &mut output,
    };
    collector.statement(body, &Scope::new(&memory), "Statement")?;
    let (nodes, memory) = output.finish()?.into_parts();
    Ok(control.finish(
        ExplainPhysicalPlan {
            nodes,
            ..ExplainPhysicalPlan::default()
        },
        memory,
    )?)
}

struct Collector<'a, 'c, 'o> {
    context: PhysicalExplainContext<'a>,
    params: &'a [SQLParam],
    control: ProductionControl<'c>,
    scope_memory: &'c MemoryBudget,
    costs: bool,
    output: &'o mut ProductionVec<'c, ExplainValue>,
}

impl Collector<'_, '_, '_> {
    fn filter_scope<'a>(&'a self, visible: &'a dyn Fn(&str) -> bool) -> FilterPushdownScope<'a> {
        FilterPushdownScope {
            catalog: self.context.filters.correlation.catalog,
            resolution: self.context.filters.correlation.resolution,
            is_visible_cte: visible,
        }
    }
}

fn visible_cte(scope: &Scope<'_>, name: &str) -> bool {
    uqa_sql::semantics::cte_reference_name(name)
        .is_some_and(|name| scope.get(name.as_str()).is_some())
}
