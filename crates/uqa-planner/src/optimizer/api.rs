//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optimizer configuration, statistics seam, and public entry points.

use super::{
    optimize_unified_plan, reorder_unified_plan_joins, AggregateClassifier, JoinGraphResult,
    RelationStats, ScalarExpr, SourcePlan, UnifiedPlan,
};
use crate::LocalAccessEstimate;

#[derive(Debug, Clone)]
pub struct OptimizerConfig {
    pub enable_filter_pushdown: bool,
    pub enable_boolean_simplify: bool,
    pub enable_vector_threshold_merge: bool,
    pub enable_join_reordering: bool,
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            enable_filter_pushdown: true,
            enable_boolean_simplify: true,
            enable_vector_threshold_merge: true,
            enable_join_reordering: true,
        }
    }
}

/// Cardinality and column statistics used to cost base relations during
/// join enumeration. Engines implement this against their live catalogue;
/// callers without a catalogue still get deterministic DPccp enumeration
/// from the optimizer's fallback cardinality.
pub trait SourceStatistics {
    fn relation_statistics(&self, table: &str) -> Option<RelationStats>;

    /// Estimate a non-table source that can participate as one atom in an
    /// inner-join region. Returning `None` keeps that region in SQL order.
    fn source_access_estimate(&self, _source: &SourcePlan) -> Option<LocalAccessEstimate> {
        None
    }

    /// Estimate a predicate that references only `table`. Returning `None`
    /// delegates to the planner's scalar selectivity model.
    fn local_access_estimate(
        &self,
        _table: &str,
        _predicate: &ScalarExpr,
    ) -> Option<LocalAccessEstimate> {
        None
    }
}

impl<F> SourceStatistics for F
where
    F: Fn(&str) -> Option<RelationStats>,
{
    fn relation_statistics(&self, table: &str) -> Option<RelationStats> {
        self(table)
    }
}

struct NoSourceStatistics;

impl SourceStatistics for NoSourceStatistics {
    fn relation_statistics(&self, _table: &str) -> Option<RelationStats> {
        None
    }
}

struct NoRegisteredAggregates;

impl AggregateClassifier for NoRegisteredAggregates {
    fn is_registered_aggregate(&self, _name: &str) -> bool {
        false
    }
}

/// Optimize a fully lowered plan using the built-in aggregate catalogue.
pub fn optimize(plan: UnifiedPlan, config: &OptimizerConfig) -> JoinGraphResult<UnifiedPlan> {
    optimize_with_aggregates_and_statistics(
        plan,
        config,
        &NoRegisteredAggregates,
        &NoSourceStatistics,
    )
}

/// Optimize a fully lowered plan while classifying engine-local aggregates.
pub fn optimize_with_aggregates(
    plan: UnifiedPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) -> JoinGraphResult<UnifiedPlan> {
    optimize_with_aggregates_and_statistics(plan, config, aggregates, &NoSourceStatistics)
}

/// Optimize a fully lowered plan with caller-provided relation statistics.
pub fn optimize_with_statistics(
    plan: UnifiedPlan,
    config: &OptimizerConfig,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<UnifiedPlan> {
    optimize_with_aggregates_and_statistics(plan, config, &NoRegisteredAggregates, statistics)
}

/// Optimize a fully lowered plan while classifying engine-local aggregates
/// and costing join orders from the engine's relation statistics.
pub fn optimize_with_aggregates_and_statistics(
    mut plan: UnifiedPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<UnifiedPlan> {
    optimize_unified_plan(&mut plan, config, aggregates);
    if config.enable_join_reordering {
        reorder_unified_plan_joins(&mut plan, statistics)?;
    }
    Ok(plan)
}
