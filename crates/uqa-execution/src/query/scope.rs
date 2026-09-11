//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-scoped CTE buffers, subquery arenas, and tuple recheck state.

use crate::catalog::CatalogReadView;
use crate::row_locks::recheck::{
    recheck_storage_names_match, RecheckDoc, RecheckSourceRow, RowLockRecheckPins,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{atomic::AtomicU64, Arc};
use uqa_sql::catalog::resolution::{RelationLookupMode, RelationNameResolution};
use uqa_sql::plan::{CtePlan, QueryPlan};
use uqa_sql::SQLError;

mod cte;
mod row_locks;
pub mod subqueries;
use row_locks::RowLockScopeState;
pub use row_locks::{LockIdentityOptions, ResolvedRowLock};
use subqueries::ScalarSubqueryCacheEntry;

#[derive(Clone)]
pub struct CteScope<S: Clone = ()> {
    pub rows: BTreeMap<String, crate::SharedSpill>,
    deferred_ctes: BTreeMap<String, CtePlan>,
    pub non_returning_ctes: BTreeSet<String>,
    pub scalar_subqueries: Vec<QueryPlan>,
    pub lock_identities: LockIdentityOptions,
    row_lock: Option<Box<RowLockScopeState>>,
    visible_cte_names: BTreeSet<String>,
    recursive_control_widths: BTreeMap<String, usize>,
    scalar_subquery_arena: u64,
    read_command_overlay: bool,
    stream_command_progress: bool,
    scan_backwards: bool,
    next_scalar_subquery_arena: Arc<AtomicU64>,
    scalar_subquery_cache:
        Arc<parking_lot::Mutex<BTreeMap<(u64, usize), ScalarSubqueryCacheEntry>>>,
    catalog: Option<CatalogReadView>,
    catalog_resolution: Option<RelationNameResolution>,
    privilege_subject: Option<String>,
    command_cte_snapshot: Option<Arc<S>>,
}

impl<S: Clone> Default for CteScope<S> {
    fn default() -> Self {
        Self {
            rows: BTreeMap::new(),
            deferred_ctes: BTreeMap::new(),
            non_returning_ctes: BTreeSet::new(),
            scalar_subqueries: Vec::new(),
            lock_identities: LockIdentityOptions::default(),
            row_lock: None,
            visible_cte_names: BTreeSet::new(),
            recursive_control_widths: BTreeMap::new(),
            scalar_subquery_arena: 0,
            read_command_overlay: true,
            stream_command_progress: false,
            scan_backwards: false,
            next_scalar_subquery_arena: Arc::new(AtomicU64::new(1)),
            scalar_subquery_cache: Arc::new(parking_lot::Mutex::new(BTreeMap::new())),
            catalog: None,
            catalog_resolution: None,
            privilege_subject: None,
            command_cte_snapshot: None,
        }
    }
}

impl<S: Clone> CteScope<S> {
    /// Bind immutable statement metadata while keeping the snapshot payload owned by the caller.
    pub fn with_catalog(
        catalog: CatalogReadView,
        resolution: RelationNameResolution,
        privilege_subject: Option<String>,
    ) -> Self {
        Self {
            catalog: Some(catalog),
            catalog_resolution: Some(resolution),
            privilege_subject,
            ..Self::default()
        }
    }

    pub fn set_reads_command_overlay(&mut self, enabled: bool) {
        self.read_command_overlay = enabled;
    }

    pub fn set_relation_lookup_mode(&mut self, mode: RelationLookupMode) -> Result<(), SQLError> {
        self.catalog_resolution
            .as_mut()
            .ok_or_else(|| {
                SQLError::Internal(
                    "query execution scope has no statement name-resolution snapshot".into(),
                )
            })?
            .set_lookup_mode(mode);
        Ok(())
    }

    pub fn cached_subquery(&self, slot: usize) -> Option<ScalarSubqueryCacheEntry> {
        self.scalar_subquery_cache
            .lock()
            .get(&(self.scalar_subquery_arena, slot))
            .cloned()
    }

    pub fn cache_subquery(&self, slot: usize, entry: ScalarSubqueryCacheEntry) {
        self.scalar_subquery_cache
            .lock()
            .insert((self.scalar_subquery_arena, slot), entry);
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn catalog_read_view(&self) -> Result<CatalogReadView, SQLError> {
        self.catalog.clone().ok_or_else(|| {
            SQLError::Internal("query execution scope has no statement catalog snapshot".into())
        })
    }

    pub fn relation_name_resolution(&self) -> Result<RelationNameResolution, SQLError> {
        self.catalog_resolution.clone().ok_or_else(|| {
            SQLError::Internal(
                "query execution scope has no statement name-resolution snapshot".into(),
            )
        })
    }

    pub fn privilege_subject(&self) -> Result<&str, SQLError> {
        if let Some(subject) = self.privilege_subject.as_deref() {
            return Ok(subject);
        }
        self.catalog_resolution
            .as_ref()
            .map(RelationNameResolution::current_user)
            .ok_or_else(|| {
                SQLError::Internal(
                    "query execution scope has no statement authorization subject".into(),
                )
            })
    }
}

pub use uqa_sql::semantics::expr_contains_subquery;

#[cfg(test)]
mod tests {
    use super::{expr_contains_subquery, CteScope};
    use crate::ScalarExpr;
    use crate::ScalarFrameBound;
    use uqa_sql::ast::FrameMode;

    #[test]
    fn empty_row_lock_scopes_leave_non_locking_state_unallocated() {
        let mut ctes = CteScope::<()>::new();
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
            spec: crate::ScalarWindowSpec {
                partition_by: Vec::new(),
                order_by: Vec::new(),
                frame: Some(crate::ScalarWindowFrame {
                    mode: FrameMode::Rows,
                    start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::ScalarSubquery(0))),
                    end: ScalarFrameBound::CurrentRow,
                }),
            },
        };
        assert!(expr_contains_subquery(&expression));
    }
    #[test]
    fn scalar_subquery_scope_restores_the_parent_arena_after_unwind() {
        use uqa_sql::plan::UnifiedPlan;

        let statement = uqa_sql::compile("SELECT 1")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
            panic!("SELECT must lower to a query plan");
        };
        let query = *query;
        let mut scope = CteScope::<()>::new();
        scope.scalar_subqueries.push(query.clone());

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = scope.enter_scalar_subqueries(&[query.clone(), query]);
            panic!("exercise scalar-subquery scope cleanup");
        }));

        assert!(unwind.is_err());
        assert_eq!(scope.scalar_subqueries.len(), 1);
    }
}

pub mod bindings;
