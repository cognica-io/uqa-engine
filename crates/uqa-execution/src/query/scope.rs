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
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::catalog::resolution::{RelationLookupMode, RelationNameResolution};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::SQLError;

pub(crate) mod binding;
mod cte;
mod definitions;
mod producers;
mod production;
pub use production::{CteRowsIter, CteRowsMap};
#[cfg(test)]
mod query_arenas;
#[cfg(test)]
mod retained_plans;
mod row_locks;
mod statement_commands;
pub mod subqueries;
use row_locks::RowLockScopeState;
pub use row_locks::{LockIdentityOptions, ResolvedRowLock};
pub use statement_commands::StatementCommands;
use subqueries::ScalarSubqueryCacheEntry;

type SourceOuterRewrites =
    Arc<uqa_core::memory::Produced<Vec<uqa_sql::semantics::view_rewrite::DeferredOuterRewrite>>>;

type ScalarSubqueryCache = parking_lot::Mutex<BTreeMap<(u64, usize), ScalarSubqueryCacheEntry>>;

#[derive(Clone)]
pub struct CteScope<S: Clone = ()> {
    producers: producers::CteProducers,
    pub rows: CteRowsMap,
    deferred_ctes: BTreeMap<String, uqa_sql::plan::RetainedCtePlan>,
    definition_scopes: BTreeMap<String, Arc<definitions::DefinitionBindings>>,
    folded_ctes: BTreeSet<String>,
    pub non_returning_ctes: BTreeSet<String>,
    pub scalar_subqueries: uqa_sql::plan::RetainedQueryArena,
    pub(crate) source_outer_rewrites: Option<SourceOuterRewrites>,
    pub lock_identities: LockIdentityOptions,
    row_lock: Option<Box<RowLockScopeState>>,
    visible_cte_names: BTreeSet<String>,
    recursive_control_widths: BTreeMap<String, usize>,
    scalar_subquery_arena: u64,
    read_command_overlay: bool,
    stream_command_progress: bool,
    scan_backwards: bool,
    next_scalar_subquery_arena: Arc<Produced<AtomicU64>>,
    scalar_subquery_cache: Arc<Produced<ScalarSubqueryCache>>,
    catalog: Option<CatalogReadView>,
    catalog_revision: Option<(u64, u64)>,
    catalog_resolution: Option<production::resolution::ScopeResolution>,
    privilege_subject: Option<production::subject::ScopeSubject>,
    command_cte_snapshot: Option<Arc<S>>,
    /// The commands of the statement this scope belongs to, shared by every scope of the statement.
    statement_commands: Option<Arc<StatementCommands>>,
}

impl<S: Clone> Default for CteScope<S> {
    fn default() -> Self {
        Self::empty_with_control(&ProductionControl::uncontrolled())
            .expect("uncontrolled query scope construction")
    }
}

impl<S: Clone> CteScope<S> {
    /// Bind immutable statement metadata while keeping the snapshot payload owned by the caller.
    pub fn with_catalog(
        catalog: CatalogReadView,
        resolution: RelationNameResolution,
        privilege_subject: Option<RoleReference>,
    ) -> Self {
        Self {
            catalog: Some(catalog),
            catalog_resolution: Some(resolution.into()),
            privilege_subject: privilege_subject.map(Into::into),
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

    /// Record the caller's source identity and invalidation revision when it captures the catalog, independently of the immutable binding view.
    pub fn set_catalog_revision(&mut self, source: u64, revision: u64) {
        self.catalog_revision = Some((source, revision));
    }

    pub fn catalog_revision(&self) -> Option<(u64, u64)> {
        self.catalog_revision
    }

    pub fn relation_name_resolution(&self) -> Result<RelationNameResolution, SQLError> {
        self.catalog_resolution
            .as_ref()
            .map(production::resolution::ScopeResolution::snapshot)
            .ok_or_else(|| {
                SQLError::Internal(
                    "query execution scope has no statement name-resolution snapshot".into(),
                )
            })
    }

    pub(crate) fn relation_lookup_mode(
        &self,
    ) -> Option<uqa_sql::catalog::resolution::RelationLookupMode> {
        self.catalog_resolution
            .as_ref()
            .map(production::resolution::ScopeResolution::lookup_mode)
    }

    pub fn privilege_subject(&self) -> Result<&RoleReference, SQLError> {
        if let Some(subject) = self.privilege_subject.as_ref() {
            return Ok(subject);
        }
        self.invoking_role()
    }

    /// The statement's invoking identity is independent of relation-owner overrides in enclosing views.
    pub fn invoking_role(&self) -> Result<&RoleReference, SQLError> {
        self.catalog_resolution
            .as_ref()
            .map(|resolution| resolution.current_user())
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
    use uqa_sql::ast::{FrameExclusion, FrameMode};

    #[test]
    fn query_scope_restores_relation_authority_and_namespace_after_unwind() {
        use uqa_sql::catalog::resolution::{RelationLookupMode, RelationNameResolution};
        let uqa_sql::plan::UnifiedPlan::Query(mut query) =
            uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
        else {
            panic!("query")
        };
        query.relation_privilege_subject = Some("caller".into());
        let mut scope = CteScope::<()>::new();
        scope.privilege_subject = Some("action_owner".into());
        scope.catalog_resolution = Some(
            RelationNameResolution {
                search_path: vec!["public".into()],
                temporary_schema: "pg_temp_1".into(),
                temporary_namespace_allocated: false,
                current_user: "caller".into(),
                lookup_mode: RelationLookupMode::Bound,
            }
            .into(),
        );
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let current = scope.enter_query_plan_scope(&query).unwrap();
            assert_eq!(current.privilege_subject().unwrap(), &"caller".into());
            let resolution = current.relation_name_resolution().unwrap();
            assert_eq!(resolution.lookup_mode, RelationLookupMode::Dynamic);
            assert_eq!(resolution.current_user, "caller".into());
            panic!("query failed while retaining caller authority");
        }));
        assert!(unwind.is_err());
        assert_eq!(scope.privilege_subject().unwrap(), &"action_owner".into());
        assert_eq!(
            scope.relation_name_resolution().unwrap().lookup_mode,
            RelationLookupMode::Bound
        );
    }

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
                definition: None,
                partition_by: Vec::new(),
                order_by: Vec::new(),
                frame: Some(crate::ScalarWindowFrame {
                    mode: FrameMode::Rows,
                    start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::ScalarSubquery(0))),
                    end: ScalarFrameBound::CurrentRow,
                    between: true,
                    exclusion: FrameExclusion::NoOthers,
                }),
            },
            filter: None,
            modifiers: uqa_sql::ast::WindowCallModifiers::default(),
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
        scope.scalar_subqueries = vec![query.clone()].into();

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = scope.enter_scalar_subqueries(&[query.clone(), query]);
            panic!("exercise scalar-subquery scope cleanup");
        }));

        assert!(unwind.is_err());
        assert_eq!(scope.scalar_subqueries.len(), 1);
    }
}

pub mod bindings;
