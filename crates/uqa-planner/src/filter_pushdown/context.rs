//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only semantic and catalog inputs for predicate placement.

use uqa_sql::{
    binding::correlation::CorrelationContext,
    catalog::{analysis::AnalysisCatalog, resolution::RelationNameResolution},
    plan::UnifiedPlan,
    semantics::volatility::VolatilityCatalog,
    SQLError,
};

#[derive(Clone, Copy)]
pub struct FilterPushdownContext<'a> {
    pub volatility: &'a dyn VolatilityCatalog,
    pub correlation: CorrelationContext<'a>,
    pub optimizer: &'a dyn Fn(UnifiedPlan) -> Result<UnifiedPlan, SQLError>,
}

#[derive(Clone, Copy)]
pub struct FilterPushdownScope<'a> {
    pub catalog: &'a dyn AnalysisCatalog,
    pub resolution: &'a RelationNameResolution,
    pub is_visible_cte: &'a dyn Fn(&str) -> bool,
}
