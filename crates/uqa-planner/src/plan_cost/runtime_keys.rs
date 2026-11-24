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
        let Some((field, keys)) = block.r#where.as_ref().and_then(SubqueryKeys::predicate) else {
            return Ok(None);
        };
        if !column_aliases.is_empty() || scope.and_then(|scope| scope.resolve(name)).is_some() {
            return Ok(None);
        }
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
        let placed = self
            .statistics
            .placed_filters(block, source, &self.control)?;
        if placed
            .as_ref()
            .and_then(|filters| filters.local.get(source.visible_qualifier()?))
            .is_none()
            || self.statistics.subquery_key_domain(inner, &self.control)? != Some(column.domain)
        {
            return Ok(None);
        }
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
            Ok(PlanCost {
                rows: keys_estimate,
                execution,
                startup: execution,
                relations: 1,
            })
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
