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
use std::{collections::BTreeMap, rc::Rc};
use uqa_sql::{
    plan::{CtePlan, UnifiedPlan},
    result::ExplainPhysicalPlan,
    retrieval::ConstantEvaluator,
    SQLError, SQLParam,
};

mod query;
mod source;
mod vector;

#[derive(Clone, Copy)]
pub struct PhysicalExplainContext<'a> {
    pub retrieval: &'a dyn RetrievalPlanningCatalog,
    pub statistics: &'a dyn PlannerStatisticsCatalog,
    pub filters: FilterPushdownContext<'a>,
    pub evaluate: &'a ConstantEvaluator<'a>,
    /// Validate the retained provider metadata for a concrete source after CTE and view expansion.
    pub validate_source: &'a dyn Fn(
        &uqa_sql::catalog::resolution::RelationNameResolution,
        &str,
    ) -> Result<(), SQLError>,
}

#[derive(Clone)]
struct DeferredCte {
    plan: CtePlan,
    outer: Rc<Scope>,
}
type Scope = BTreeMap<String, Option<DeferredCte>>;

pub fn physical_plan(
    context: PhysicalExplainContext<'_>,
    body: &UnifiedPlan,
    params: &[SQLParam],
) -> Result<ExplainPhysicalPlan, SQLError> {
    let mut collector = Collector {
        context,
        params,
        output: ExplainPhysicalPlan::default(),
    };
    collector.statement(body, &Scope::new(), "Statement")?;
    Ok(collector.output)
}

struct Collector<'a> {
    context: PhysicalExplainContext<'a>,
    params: &'a [SQLParam],
    output: ExplainPhysicalPlan,
}

impl Collector<'_> {
    fn filter_scope<'a>(&'a self, visible: &'a dyn Fn(&str) -> bool) -> FilterPushdownScope<'a> {
        FilterPushdownScope {
            catalog: self.context.filters.correlation.catalog,
            resolution: self.context.filters.correlation.resolution,
            is_visible_cte: visible,
        }
    }
}

fn visible_cte(scope: &Scope, name: &str) -> bool {
    uqa_sql::semantics::cte_reference_name(name).is_some_and(|name| scope.contains_key(&name))
}
