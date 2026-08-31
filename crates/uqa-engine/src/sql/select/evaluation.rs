//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped scalar, function, and subquery evaluation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{atomic::AtomicU64, Arc};

use uqa_planner::{CtePlan, QueryPlan};
use uqa_sql::SQLError;

use super::{
    recheck_storage_names_match, RecheckDoc, RecheckSourceRow, ResolvedRowLock, RowLockRecheckPins,
    ScalarExpr,
};
use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};

#[path = "evaluation/callbacks.rs"]
mod callbacks;
#[path = "evaluation/cte.rs"]
mod cte;
#[path = "evaluation/row_locks.rs"]
mod row_locks;
#[path = "evaluation/subqueries.rs"]
mod subqueries;
#[path = "evaluation/type_resolution.rs"]
mod type_resolution;

pub(in crate::sql) use callbacks::{
    prepare_correlated_exists_predicate, EngineExpressionEvaluator, ScopedEngineHook,
};
use row_locks::{LockIdentityOptions, RowLockScopeState};
pub(in crate::sql) use subqueries::DirectColumnKey;
use subqueries::ScalarSubqueryCacheEntry;

#[derive(Clone)]
pub(crate) struct CteScope {
    pub(in crate::sql) rows: BTreeMap<String, uqa_execution::SharedSpill>,
    deferred_ctes: BTreeMap<String, CtePlan>,
    pub(in crate::sql) scalar_subqueries: Vec<QueryPlan>,
    pub(in crate::sql) lock_identities: LockIdentityOptions,
    row_lock: Option<Box<RowLockScopeState>>,
    visible_cte_names: BTreeSet<String>,
    recursive_control_widths: BTreeMap<String, usize>,
    scalar_subquery_arena: u64,
    read_command_overlay: bool,
    stream_command_progress: bool,
    next_scalar_subquery_arena: Arc<AtomicU64>,
    scalar_subquery_cache:
        Arc<parking_lot::Mutex<BTreeMap<(u64, usize), ScalarSubqueryCacheEntry>>>,
    catalog: Option<CatalogReadView>,
    catalog_resolution: Option<RelationNameResolution>,
}

impl Default for CteScope {
    fn default() -> Self {
        Self {
            rows: BTreeMap::new(),
            deferred_ctes: BTreeMap::new(),
            scalar_subqueries: Vec::new(),
            lock_identities: LockIdentityOptions::default(),
            row_lock: None,
            visible_cte_names: BTreeSet::new(),
            recursive_control_widths: BTreeMap::new(),
            scalar_subquery_arena: 0,
            read_command_overlay: true,
            stream_command_progress: false,
            next_scalar_subquery_arena: Arc::new(AtomicU64::new(1)),
            scalar_subquery_cache: Arc::new(parking_lot::Mutex::new(BTreeMap::new())),
            catalog: None,
            catalog_resolution: None,
        }
    }
}

impl CteScope {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(in crate::sql) fn catalog_read_view(&self) -> Result<CatalogReadView, SQLError> {
        self.catalog.clone().ok_or_else(|| {
            SQLError::Internal("query execution scope has no statement catalog snapshot".into())
        })
    }

    pub(in crate::sql) fn relation_name_resolution(
        &self,
    ) -> Result<RelationNameResolution, SQLError> {
        self.catalog_resolution.clone().ok_or_else(|| {
            SQLError::Internal(
                "query execution scope has no statement name-resolution snapshot".into(),
            )
        })
    }
}

pub(in crate::sql) fn expr_contains_subquery(expr: &ScalarExpr) -> bool {
    expr.contains_subquery()
}

#[cfg(test)]
mod tests {
    use super::{expr_contains_subquery, CteScope, ScalarExpr};
    use uqa_execution::ScalarFrameBound;
    use uqa_sql::ast::FrameMode;

    #[test]
    fn empty_row_lock_scopes_leave_non_locking_state_unallocated() {
        let mut ctes = CteScope::new();
        {
            let _scope = ctes.enter_source_row_locks(Vec::new());
        }
        assert!(ctes.row_lock.is_none());
        {
            let _scope = ctes.enter_recheck_storage_pins("plain_source");
        }
        assert!(ctes.row_lock.is_none());
    }

    #[test]
    fn scalar_ir_detects_subqueries_in_window_frame_bounds() {
        let expression = ScalarExpr::WindowCall {
            name: "sum".into(),
            args: vec![ScalarExpr::Column("amount".into())],
            spec: uqa_execution::ScalarWindowSpec {
                partition_by: Vec::new(),
                order_by: Vec::new(),
                frame: Some(uqa_execution::ScalarWindowFrame {
                    mode: FrameMode::Rows,
                    start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::ScalarSubquery(0))),
                    end: ScalarFrameBound::CurrentRow,
                }),
            },
        };
        assert!(expr_contains_subquery(&expression));
    }
}
