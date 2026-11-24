//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Account for the once-per-statement key producer and per-outer-row index operations.

use super::{
    Capture, JoinExecutionStrategy, OperatorKind, PlanCost, PlanCostEstimator, PlanCostInput,
    QueryBlockPlan, SQLError, Scope, SourcePlan,
};
use uqa_sql::catalog::index::lookup::SubqueryKeys;

impl PlanCostEstimator<'_, '_> {
    pub(super) fn runtime_source<'p>(
        &self,
        block: &'p QueryBlockPlan,
        source: &'p SourcePlan,
        scope: Scope<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<Option<(PlanCost, Option<usize>)>, SQLError> {
        if contains_index_join(source) {
            let filters = self
                .statistics
                .placed_filters(block, source, &self.control)?;
            let Some(filters) = filters else {
                return Ok(None);
            };
            let cost = self.source(
                source,
                filters.residual.as_deref(),
                scope,
                capture,
                Some(&filters.local),
            )?;
            return Ok(Some((cost, None)));
        }
        let SourcePlan::Table {
            name,
            executable_query: None,
            column_aliases,
            ..
        } = source
        else {
            return Ok(None);
        };
        let Some(predicate) = &block.r#where else {
            return Ok(None);
        };
        if !column_aliases.is_empty()
            || scope.and_then(|scope| scope.resolve(name)).is_some()
            || SubqueryKeys::first_in_filter(predicate, &self.control)?.is_none()
        {
            return Ok(None);
        }
        let Some(placed) = self
            .statistics
            .placed_filters(block, source, &self.control)?
        else {
            return Ok(None);
        };
        let Some(terms) = source
            .visible_qualifier()
            .and_then(|name| placed.local.get(name))
        else {
            return Ok(None);
        };
        let bound = SubqueryKeys::find_usable(terms, &self.control, |field, keys| {
            let Some(column) = self
                .statistics
                .lookup_column(name, field)
                .filter(|column| column.unique)
            else {
                return Ok(None);
            };
            let Some(inner) = block.subqueries.get(keys.slot()) else {
                return Ok(None);
            };
            let domain = self.statistics.subquery_key_domain(inner, &self.control)?;
            Ok((domain == Some(column.domain)).then_some((field, keys, inner)))
        })?;
        let Some((field, keys, inner)) = bound else {
            return Ok(None);
        };
        let child = self.query(inner, scope, capture)?;
        let keys_estimate = match keys {
            SubqueryKeys::Scalar(_) => 1.0,
            SubqueryKeys::Membership(_) => child.rows,
        };
        let mut outer = Capture::node(capture, PlanCostInput::Source(source), || {
            if keys_estimate > uqa_sql::catalog::index::lookup::MAX_MEMBERSHIP_PROBES as f64 {
                return self.table(name, block.r#where.as_ref());
            }
            let execution = self.unary(OperatorKind::IndexScan, keys_estimate)
                + self.unary(OperatorKind::Filter, keys_estimate);
            let mut estimate = PlanCost {
                rows: keys_estimate,
                execution,
                startup: execution,
                relations: 1,
            };
            for term in terms {
                if SubqueryKeys::predicate(term) != Some((field, keys)) {
                    self.filter(&mut estimate, Some(term), &super::RelationStats::default())?;
                }
            }
            self.filter(
                &mut estimate,
                placed.residual.as_deref(),
                &super::RelationStats::default(),
            )?;
            Ok(estimate)
        })?;
        outer.startup += child.execution;
        outer.add_child(child);
        Ok(Some((outer, Some(keys.slot()))))
    }

    pub(super) fn index_join<'p>(
        &self,
        mut outer: PlanCost,
        inner: &'p SourcePlan,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        let SourcePlan::Table { name, .. } = inner else {
            return Err(SQLError::Internal("index join requires a table".into()));
        };
        let size = self
            .statistics
            .relation_statistics(name)
            .map_or(super::DEFAULT_ROWS, |stats| stats.row_count as f64);
        let probe = Capture::node(capture, PlanCostInput::Source(inner), || {
            let execution = self
                .costs
                .estimate_join(OperatorKind::IndexJoin, 1.0, size)
                .total();
            Ok(PlanCost {
                rows: 1.0,
                startup: execution,
                execution,
                relations: 1,
            })
        })?;
        outer.startup += probe.startup.min(outer.rows * probe.execution);
        outer.execution += outer.rows * probe.execution;
        outer.relations += probe.relations;
        Ok(outer)
    }
}

fn contains_index_join(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Join {
            left,
            right,
            strategy,
            kind: uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross,
            alias: None,
            ..
        } => {
            matches!(strategy, JoinExecutionStrategy::IndexNestedLoop { .. })
                || contains_index_join(left)
                || contains_index_join(right)
        }
        _ => false,
    }
}
