//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics-driven DPccp join reordering and predicate attachment.

use super::{
    BTreeMap, BTreeSet, BinaryOp, CommandPlan, JoinGraphError, JoinOrderOptimizer, JoinPredicate,
    RelationalPlan, ScalarExpr, ScalarFrameBound, SourcePlan, SourceStatistics,
};

use uqa_core::memory::{Produced, ProductionControl, ProductionSmallVec, ProductionVec};

use super::OptimizerResult;

const DEFAULT_JOIN_CARDINALITY: u64 = 1_000;

#[cfg(test)]
mod tests;
#[cfg(test)]
use crate::UnifiedPlan;

mod bindings;
mod scope;
mod selection;
mod traversal;
pub(super) use traversal::{reorder_caller_plan, reorder_owned_plan};
mod inference;
mod inputs;
mod output;
use inference::{implied_join_predicates, join_predicate};
#[cfg(test)]
use output::Output;
mod materialization;
mod relations;
use inputs::{
    collect_conjuncts, flatten_reorderable_inner_join, local_predicate_groups, ColumnOwners,
};
use relations::RelationMetadata;

fn copy_local_conjunction(
    predicates: &[&ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<Produced<ScalarExpr>, uqa_sql::SQLError> {
    let mut expressions = ProductionVec::new(*control);
    expressions.reserve(predicates.len())?;
    for predicate in predicates {
        expressions.push_produced(predicate.clone_with_control(control)?)?;
    }
    let (expressions, memory) = expressions.finish()?.into_parts();
    Ok(control.finish(ScalarExpr::And(expressions), memory)?)
}

#[derive(Debug)]
struct LocalFilters {
    estimates: [Option<crate::LocalAccessEstimate>; 64],
    retrieval_sources: u64,
}

fn estimate_local_filters(
    relations: &[RelationMetadata<'_>],
    predicates: &[&ScalarExpr],
    column_owners: &ColumnOwners<'_>,
    relation_tables: &[Option<&str>],
    source_statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<LocalFilters> {
    let estimator = crate::CardinalityEstimator::new();
    let mut estimates = [None; 64];
    let mut retrieval_sources = 0;
    let predicates_by_alias =
        local_predicate_groups(relations, predicates, column_owners, control)?;
    for (index, (((relation, predicates), table), output)) in relations
        .iter()
        .zip(predicates_by_alias)
        .zip(relation_tables)
        .zip(&mut estimates)
        .enumerate()
    {
        let predicates = predicates.expect("one predicate bucket per relation");
        if predicates.is_empty() {
            continue;
        }
        let conjunction;
        let predicate = if let [predicate] = &*predicates {
            *predicate
        } else {
            conjunction = copy_local_conjunction(&predicates, control)?;
            &conjunction
        };
        // Relation-wide retrieval selects its own candidates before any outer-row key can restrict this input.
        if uqa_sql::semantics::contains_retrieval(predicate) {
            retrieval_sources |= 1_u64 << index;
        }
        let statistics = crate::cardinality::RelationStatsRef {
            row_count: relation.cardinality.round() as u64,
            columns: &relation.column_stats,
        };
        let local_estimate =
            table.and_then(|table| source_statistics.local_access_estimate(table, predicate));
        if let Some(estimate) = local_estimate {
            if !estimate.output_rows.is_finite() || estimate.output_rows < 0.0 {
                return Err(JoinGraphError::InvalidCardinality {
                    name: relation.alias.to_string(),
                    rows: estimate.output_rows,
                }
                .into());
            }
            if !estimate.cost.is_finite() || estimate.cost < 0.0 {
                return Err(JoinGraphError::InvalidAccessCost {
                    name: relation.alias.to_string(),
                    cost: estimate.cost,
                }
                .into());
            }
            *output = Some(crate::LocalAccessEstimate {
                output_rows: estimate.output_rows.min(relation.cardinality).max(0.0),
                ..estimate
            });
            continue;
        }
        let selectivity = estimator
            .scalar_selectivity_with_statistics(predicate, statistics, control)?
            .raw();
        *output = Some(crate::LocalAccessEstimate {
            output_rows: (relation.cardinality * selectivity).max(0.0),
            cost: relation.access_cost
                + crate::CostEstimator::default()
                    .estimate_unary(crate::OperatorKind::Filter, statistics.row_count as f64)
                    .total(),
            paradigm: relation.paradigm,
        });
    }
    Ok(LocalFilters {
        estimates,
        retrieval_sources,
    })
}
