//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped scalar, function, and subquery evaluation.

use super::{
    engine_func_intercept, eval_physical_scalar, execute_lateral_subquery_output,
    execute_query_plan_output, is_score_provenance_column, physical_exec_error,
    query_contains_volatile_function, recheck_storage_names_match, Arc, AtomicU64, BTreeMap,
    BTreeSet, CtePlan, Engine, ExecResult, ExpressionEvaluator, Ordering, PhysicalEvalContext,
    PhysicalOuterRow, PhysicalSubqueryRunner, QueryOutputMode, QueryPlan, QueryRows, RecheckDoc,
    RecheckSourceRow, ResolvedRowLock, RowLockRecheckPins, SQLError, SQLParam, ScalarExpr,
    ScalarFrameBound, SharedExpressionEvaluator, Value, DOC_ID_COLUMN, MERGE_ACTION_COLUMN,
    SCORE_COLUMN,
};
use uqa_execution::FunctionTypeResolver;
use uqa_sql::expr::RowLookup;

#[path = "evaluation/type_resolution.rs"]
mod type_resolution;

type RecheckStoragePin = (String, String, Arc<Vec<RecheckDoc>>);

#[derive(Clone, Copy, Default)]
pub(in crate::sql) struct LockIdentityOptions {
    pub(in crate::sql) emit: bool,
    pub(in crate::sql) retain_after_lock: bool,
}

#[derive(Clone, Default)]
struct RowLockScopeState {
    source_row_locks: Vec<ResolvedRowLock>,
    recheck: Option<Arc<RowLockRecheckPins>>,
    outer_row: Option<Arc<uqa_execution::OwnedPhysicalRow>>,
    storage_pins: Vec<RecheckStoragePin>,
}

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
        }
    }
}

impl CteScope {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn row_lock_state(&self) -> Option<&RowLockScopeState> {
        self.row_lock.as_deref()
    }

    fn row_lock_state_mut(&mut self) -> &mut RowLockScopeState {
        self.row_lock
            .get_or_insert_with(|| Box::new(RowLockScopeState::default()))
    }

    /// Enter one tuple-local recheck execution: base scans of pinned lock targets emit only the candidate's tuples and every nested `LockRows` is suppressed while lock identities keep flowing.
    pub(in crate::sql) fn activate_row_lock_recheck(&mut self, pins: Arc<RowLockRecheckPins>) {
        self.row_lock_state_mut().recheck = Some(pins);
    }

    pub(in crate::sql) fn row_lock_recheck_active(&self) -> bool {
        self.row_lock_state()
            .is_some_and(|state| state.recheck.is_some())
    }

    /// Preserve the complete correlated outer row for a tuple-local locking recheck. The rebuilt inner query must receive the same scope overlay as its original lateral execution or its correlation predicate would see NULL after a lock wait and incorrectly discard the refreshed tuple.
    pub(in crate::sql) fn set_row_lock_outer_row(&mut self, row: uqa_execution::OwnedPhysicalRow) {
        self.row_lock_state_mut().outer_row = Some(Arc::new(row));
    }

    pub(in crate::sql) fn row_lock_outer_row(&self) -> Option<&uqa_execution::OwnedPhysicalRow> {
        self.row_lock_state()?.outer_row.as_deref()
    }

    fn clear_row_lock_outer_row(&mut self) {
        if let Some(state) = self.row_lock.as_mut() {
            state.outer_row = None;
        }
    }

    /// Pinned tuples one base scan must emit during an active recheck.
    pub(in crate::sql) fn recheck_docs_for_scan(
        &self,
        qualifier: &str,
        storage_name: &str,
    ) -> Option<Arc<Vec<RecheckDoc>>> {
        let state = self.row_lock_state()?;
        if let Some(pins) = state.recheck.as_ref() {
            if let Some(docs) = pins.docs_for_scan(qualifier, storage_name) {
                return Some(docs);
            }
        }
        state
            .storage_pins
            .iter()
            .find(|(pinned_storage, pinned_scan, _)| {
                pinned_scan == qualifier
                    && recheck_storage_names_match(pinned_storage, storage_name)
            })
            .map(|(_, _, docs)| Arc::clone(docs))
    }

    /// Exact copy-row mark for one top-level FROM leaf in the active tuple recheck. Paths use 0/1 for left/right join descent and are scoped to the original `LockRows` source, so nested query aliases cannot collide.
    pub(in crate::sql) fn recheck_source_row(&self, path: &[u8]) -> Option<RecheckSourceRow> {
        self.row_lock_state()?
            .recheck
            .as_ref()
            .and_then(|pins| pins.source_row(path))
    }

    /// Enter the build of one identity-source lock target's subtree so every base scan of its storage inside the derived table or view is pinned. Pins already active from an enclosing target stay active: the target's base scans may sit below further derived-table boundaries.
    pub(in crate::sql) fn enter_recheck_storage_pins(
        &mut self,
        qualifier: &str,
    ) -> RecheckStoragePinScope<'_> {
        let added = self
            .row_lock_state()
            .and_then(|state| state.recheck.as_ref())
            .map(|pins| pins.storage_pins_for_identity_source(qualifier))
            .unwrap_or_default();
        let previous = if added.is_empty() {
            None
        } else {
            let state = self.row_lock_state_mut();
            let previous = state.storage_pins.clone();
            state.storage_pins.extend(added);
            Some(previous)
        };
        RecheckStoragePinScope {
            ctes: self,
            previous,
        }
    }

    pub(in crate::sql) fn insert_shared(&mut self, name: String, rows: uqa_execution::SharedSpill) {
        self.deferred_ctes.remove(&name);
        self.rows.insert(name, rows);
    }

    pub(in crate::sql) fn insert_deferred(&mut self, plan: CtePlan) {
        self.rows.remove(&plan.name);
        self.deferred_ctes.insert(plan.name.clone(), plan);
    }

    pub(in crate::sql) fn remove_deferred(&mut self, name: &str) -> Option<CtePlan> {
        self.deferred_ctes.remove(name)
    }

    /// Return one deferred CTE for a scan. `NOT MATERIALIZED` definitions remain available so every syntactic reference is independently folded, while the default single-reference fast path is consumed exactly once.
    pub(in crate::sql) fn deferred_for_scan(&mut self, name: &str) -> Option<CtePlan> {
        let persistent = self.deferred_ctes.get(name).is_some_and(|plan| {
            plan.materialization == uqa_sql::ast::CteMaterialization::NotMaterialized
        });
        if persistent {
            self.deferred_ctes.get(name).cloned()
        } else {
            self.deferred_ctes.remove(name)
        }
    }

    pub(in crate::sql) fn deferred_ctes(&self) -> &BTreeMap<String, CtePlan> {
        &self.deferred_ctes
    }

    pub(in crate::sql) fn recursive_control_width(&self, name: &str) -> Option<usize> {
        self.recursive_control_widths.get(name).copied()
    }

    pub(in crate::sql) fn set_recursive_control_width(
        &mut self,
        name: String,
        width: usize,
    ) -> Option<usize> {
        self.recursive_control_widths.insert(name, width)
    }

    pub(in crate::sql) fn restore_recursive_control_width(
        &mut self,
        name: &str,
        previous: Option<usize>,
    ) {
        match previous {
            Some(width) => {
                self.recursive_control_widths
                    .insert(name.to_string(), width);
            }
            None => {
                self.recursive_control_widths.remove(name);
            }
        }
    }

    pub(in crate::sql) fn remove_materialized(
        &mut self,
        name: &str,
    ) -> Option<uqa_execution::SharedSpill> {
        self.rows.remove(name)
    }

    /// Bind the scalar-subquery arena owned by one query block. The guard
    /// restores the parent arena on success, error, or panic so nested and
    /// lateral query execution cannot resolve a child slot in its parent.
    pub(in crate::sql) fn enter_scalar_subqueries(
        &mut self,
        subqueries: &[QueryPlan],
    ) -> ScalarSubqueryScope<'_> {
        let previous = std::mem::replace(&mut self.scalar_subqueries, subqueries.to_vec());
        let next_arena = self
            .next_scalar_subquery_arena
            .fetch_add(1, Ordering::Relaxed);
        let previous_arena = std::mem::replace(&mut self.scalar_subquery_arena, next_arena);
        let previous_lock_identities = self.lock_identities;
        ScalarSubqueryScope {
            ctes: self,
            previous: Some(previous),
            previous_arena,
            previous_lock_identities,
        }
    }

    pub(in crate::sql) fn enter_lock_identity_emission(
        &mut self,
        enabled: bool,
    ) -> LockIdentityEmissionScope<'_> {
        let previous = std::mem::replace(
            &mut self.lock_identities,
            LockIdentityOptions {
                emit: enabled,
                retain_after_lock: false,
            },
        );
        LockIdentityEmissionScope {
            ctes: self,
            previous,
        }
    }

    pub(in crate::sql) fn enter_source_row_locks(
        &mut self,
        locks: Vec<ResolvedRowLock>,
    ) -> SourceRowLockScope<'_> {
        let existing_is_empty = self
            .row_lock_state()
            .is_none_or(|state| state.source_row_locks.is_empty());
        let previous = if locks.is_empty() && existing_is_empty {
            None
        } else {
            Some(std::mem::replace(
                &mut self.row_lock_state_mut().source_row_locks,
                locks,
            ))
        };
        SourceRowLockScope {
            ctes: self,
            previous,
        }
    }

    pub(in crate::sql) fn source_row_lock_for_view(
        &self,
        qualifier: &str,
        storage_name: &str,
    ) -> Option<ResolvedRowLock> {
        self.row_lock_state()?
            .source_row_locks
            .iter()
            .find(|target| {
                target.identity_source
                    && target.qualifier == qualifier
                    && target.storage_name == storage_name
            })
            .cloned()
    }

    pub(in crate::sql) fn enter_visible_ctes<'a>(
        &'a mut self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> VisibleCteScope<'a> {
        let previous = std::mem::take(&mut self.visible_cte_names);
        self.visible_cte_names.clone_from(&previous);
        self.visible_cte_names
            .extend(names.into_iter().map(str::to_owned));
        VisibleCteScope {
            ctes: self,
            previous: Some(previous),
        }
    }

    /// Whether `name` resolves to a CTE in this scope: a name declared by an enclosing query's WITH list, or one whose rows or deferred plan are bound in the scope, which is how a DML statement's own WITH list reaches the query it drives.
    pub(in crate::sql) fn is_visible_cte(&self, name: &str) -> bool {
        self.visible_cte_names.contains(name)
            || self.rows.contains_key(name)
            || self.deferred_ctes.contains_key(name)
    }

    pub(in crate::sql) fn returning_statement_snapshot_scope(&self) -> Self {
        let mut scope = self.clone();
        scope.read_command_overlay = false;
        scope
    }

    pub(in crate::sql) fn reads_command_overlay(&self) -> bool {
        self.read_command_overlay
    }

    pub(in crate::sql) fn enable_command_progress_streaming(&mut self) {
        self.stream_command_progress = true;
    }

    pub(in crate::sql) fn streams_command_progress(&self) -> bool {
        self.stream_command_progress
    }
}

pub(in crate::sql) struct ScalarSubqueryScope<'a> {
    ctes: &'a mut CteScope,
    previous: Option<Vec<QueryPlan>>,
    previous_arena: u64,
    previous_lock_identities: LockIdentityOptions,
}

impl std::ops::Deref for ScalarSubqueryScope<'_> {
    type Target = CteScope;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl std::ops::DerefMut for ScalarSubqueryScope<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl Drop for ScalarSubqueryScope<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.ctes.scalar_subqueries = previous;
            self.ctes.scalar_subquery_arena = self.previous_arena;
            self.ctes.lock_identities = self.previous_lock_identities;
        }
    }
}

pub(in crate::sql) struct LockIdentityEmissionScope<'a> {
    ctes: &'a mut CteScope,
    previous: LockIdentityOptions,
}

pub(in crate::sql) struct SourceRowLockScope<'a> {
    ctes: &'a mut CteScope,
    previous: Option<Vec<ResolvedRowLock>>,
}

pub(in crate::sql) struct RecheckStoragePinScope<'a> {
    ctes: &'a mut CteScope,
    previous: Option<Vec<RecheckStoragePin>>,
}

impl std::ops::Deref for RecheckStoragePinScope<'_> {
    type Target = CteScope;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl std::ops::DerefMut for RecheckStoragePinScope<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl Drop for RecheckStoragePinScope<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.ctes.row_lock_state_mut().storage_pins = previous;
        }
    }
}

impl std::ops::Deref for SourceRowLockScope<'_> {
    type Target = CteScope;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl std::ops::DerefMut for SourceRowLockScope<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl Drop for SourceRowLockScope<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.ctes.row_lock_state_mut().source_row_locks = previous;
        }
    }
}

impl std::ops::Deref for LockIdentityEmissionScope<'_> {
    type Target = CteScope;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl std::ops::DerefMut for LockIdentityEmissionScope<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl Drop for LockIdentityEmissionScope<'_> {
    fn drop(&mut self) {
        self.ctes.lock_identities = self.previous;
    }
}

pub(in crate::sql) struct VisibleCteScope<'a> {
    ctes: &'a mut CteScope,
    previous: Option<BTreeSet<String>>,
}

impl std::ops::Deref for VisibleCteScope<'_> {
    type Target = CteScope;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl std::ops::DerefMut for VisibleCteScope<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl Drop for VisibleCteScope<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.ctes.visible_cte_names = previous;
        }
    }
}

#[derive(Clone)]
pub(in crate::sql) enum ScalarSubqueryCacheEntry {
    Correlated,
    CorrelatedExists(Arc<CachedCorrelatedExists>),
    Materialized(CachedScalarSubquery),
    Membership(Arc<CachedSubqueryMembership>),
    Scalar(Value),
    Exists(bool),
}

pub(in crate::sql) struct CachedCorrelatedExists {
    outer_keys: CorrelatedExistsOuterKeys,
    keys: uqa_execution::CanonicalRowHashSet,
}

enum CorrelatedExistsOuterKeys {
    Direct(Vec<DirectColumnKey>),
    Evaluated(Vec<ScalarExpr>),
}

impl CorrelatedExistsOuterKeys {
    fn compile(expressions: Vec<ScalarExpr>) -> Self {
        let direct = expressions
            .iter()
            .map(DirectColumnKey::compile)
            .collect::<Option<Vec<_>>>();
        direct.map_or(Self::Evaluated(expressions), Self::Direct)
    }
}

/// A scalar key expression that can be resolved as a borrowed physical value
/// instead of cloning it through the general expression evaluator.
pub(in crate::sql) enum DirectColumnKey {
    Column(String),
    Qualified { qualifier: String, column: String },
}

impl DirectColumnKey {
    pub(in crate::sql) fn compile(expression: &ScalarExpr) -> Option<Self> {
        match expression {
            ScalarExpr::Column(column) => Some(Self::Column(column.clone())),
            ScalarExpr::QualifiedColumn { qualifier, column } => Some(Self::Qualified {
                qualifier: qualifier.clone(),
                column: column.clone(),
            }),
            _ => None,
        }
    }

    pub(in crate::sql) fn value<'a>(&self, row: &'a dyn RowLookup) -> Option<&'a Value> {
        match self {
            Self::Column(column) => row.column(column),
            Self::Qualified { qualifier, column } => row.qualified_column(qualifier, column),
        }
    }
}

pub(in crate::sql) struct CachedSubqueryMembership {
    values: parking_lot::Mutex<uqa_execution::ExactRowSet>,
    has_column: bool,
    saw_row: bool,
    saw_null: bool,
}

impl CachedSubqueryMembership {
    fn contains(&self, needle: &Value) -> Result<Option<bool>, SQLError> {
        if !self.has_column {
            return Ok(Some(false));
        }
        if !matches!(needle, Value::Null)
            && self
                .values
                .lock()
                .contains_values(std::slice::from_ref(needle))
                .map_err(physical_exec_error)?
        {
            return Ok(Some(true));
        }
        Ok(if !self.saw_row {
            Some(false)
        } else if matches!(needle, Value::Null) || self.saw_null {
            None
        } else {
            Some(false)
        })
    }
}

#[derive(Clone)]
pub(in crate::sql) struct CachedScalarSubquery {
    columns: Vec<String>,
    rows: uqa_execution::SharedSpill,
}

impl CachedScalarSubquery {
    fn result(&self) -> Result<uqa_execution::SubqueryResult, SQLError> {
        let rows = self
            .rows
            .read_rows()
            .map_err(physical_exec_error)?
            .map(|row| row.map_err(physical_exec_error));
        Ok(uqa_execution::SubqueryResult {
            columns: self.columns.clone(),
            rows: Box::new(rows),
        })
    }

    fn membership(&self, work_mem_bytes: usize) -> Result<CachedSubqueryMembership, SQLError> {
        let Some(first_column) = self.columns.first() else {
            return Ok(CachedSubqueryMembership {
                values: parking_lot::Mutex::new(uqa_execution::ExactRowSet::new(work_mem_bytes)),
                has_column: false,
                saw_row: false,
                saw_null: false,
            });
        };
        let mut values = uqa_execution::ExactRowSet::new(work_mem_bytes);
        let mut saw_row = false;
        let mut saw_null = false;
        for batch in self.rows.reader().map_err(physical_exec_error)? {
            let batch = batch.map_err(physical_exec_error)?;
            let position = batch.schema.position(first_column).ok_or_else(|| {
                SQLError::Internal(format!(
                    "cached subquery output column `{first_column}` is missing"
                ))
            })?;
            for row in &batch.rows {
                saw_row = true;
                match batch.schema.view(row).value_at(position) {
                    Some(Value::Null) | None => saw_null = true,
                    Some(value) => {
                        values
                            .insert_values(std::slice::from_ref(value))
                            .map_err(physical_exec_error)?;
                    }
                }
            }
        }
        Ok(CachedSubqueryMembership {
            values: parking_lot::Mutex::new(values),
            has_column: true,
            saw_row,
            saw_null,
        })
    }
}

pub(in crate::sql) struct ScopedEngineHook<'a> {
    engine: &'a Engine,
    ctes: &'a CteScope,
}

impl<'a> ScopedEngineHook<'a> {
    pub(in crate::sql) fn new(engine: &'a Engine, ctes: &'a CteScope) -> Self {
        Self { engine, ctes }
    }
}

/// Scalar adapter shared by Filter, Project, and Sort. It binds the engine's
/// registered functions and the query block's physical subquery arena without
/// evaluating any row expression outside the operator tree.
pub(in crate::sql) struct EngineExpressionEvaluator<'a> {
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: CteScope,
}

struct PreparedCorrelatedExistsPredicate<'a> {
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: CteScope,
    lookup: Arc<CachedCorrelatedExists>,
    negated: bool,
}

impl uqa_execution::RowPredicate for PreparedCorrelatedExistsPredicate<'_> {
    fn keep_physical(
        &self,
        schema: &uqa_execution::RowSchema,
        row: &uqa_execution::PhysicalRow,
    ) -> ExecResult<bool> {
        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let exists = hook.correlated_exists_matches(
            &self.lookup,
            PhysicalOuterRow::Physical { schema, row },
            self.params,
        )?;
        Ok(if self.negated { !exists } else { exists })
    }
}

/// Prepare a simple immutable correlated EXISTS before the outer scan starts.
/// The filter then probes its key set directly, avoiding a scalar-expression
/// walk and shared subquery-cache lock for every outer row.
pub(in crate::sql) fn prepare_correlated_exists_predicate<'a>(
    engine: &'a Engine,
    expression: &ScalarExpr,
    params: &'a [SQLParam],
    ctes: &CteScope,
) -> Result<Option<uqa_execution::SharedRowPredicate<'a>>, SQLError> {
    let ScalarExpr::Exists { subquery, negated } = expression else {
        return Ok(None);
    };
    let Some(plan) = ctes.scalar_subqueries.get(*subquery) else {
        return Err(SQLError::Internal(format!(
            "physical scalar subquery slot {subquery} is out of bounds"
        )));
    };
    if query_contains_volatile_function(engine, plan)?
        || !crate::sql::correlation::query_depends_on_outer_row(engine, plan)?
    {
        return Ok(None);
    }
    let hook = ScopedEngineHook::new(engine, ctes);
    let Some(lookup) = hook.build_correlated_exists(plan, params)? else {
        return Ok(None);
    };
    Ok(Some(Arc::new(PreparedCorrelatedExistsPredicate {
        engine,
        params,
        ctes: ctes.clone(),
        lookup,
        negated: *negated,
    })))
}

impl<'a> EngineExpressionEvaluator<'a> {
    pub(in crate::sql) fn shared(
        engine: &'a Engine,
        params: &'a [SQLParam],
        ctes: &CteScope,
    ) -> SharedExpressionEvaluator<'a> {
        Arc::new(Self {
            engine,
            params,
            ctes: ctes.clone(),
        })
    }

    fn evaluate_physical_scoped(
        &self,
        expression: &ScalarExpr,
        schema: &uqa_execution::RowSchema,
        row: &uqa_execution::PhysicalRow,
    ) -> ExecResult<Value> {
        let view = schema.view(row);
        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let context = PhysicalEvalContext::from_row_lookup(&view, self.params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook)
            .with_physical_outer_row(schema, row);
        if let ScalarExpr::Func { name, args, .. } = expression {
            let mut evaluate = |expr: &ScalarExpr| {
                eval_physical_scalar(expr, &self.ctes.scalar_subqueries, &context)
            };
            if let Some(value) =
                engine_func_intercept(Some(self.engine), name, args, &view, &mut evaluate)?
            {
                return Ok(value);
            }
        }
        Ok(eval_physical_scalar(
            expression,
            &self.ctes.scalar_subqueries,
            &context,
        )?)
    }
}

impl ExpressionEvaluator for EngineExpressionEvaluator<'_> {
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value> {
        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let context = PhysicalEvalContext::from_row_lookup(row, self.params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        if let ScalarExpr::Func { name, args, .. } = expression {
            let mut evaluate = |expr: &ScalarExpr| {
                eval_physical_scalar(expr, &self.ctes.scalar_subqueries, &context)
            };
            if let Some(value) =
                engine_func_intercept(Some(self.engine), name, args, row, &mut evaluate)?
            {
                return Ok(value);
            }
        }
        Ok(eval_physical_scalar(
            expression,
            &self.ctes.scalar_subqueries,
            &context,
        )?)
    }

    fn evaluate_physical(
        &self,
        expression: &ScalarExpr,
        schema: &uqa_execution::RowSchema,
        row: &uqa_execution::PhysicalRow,
    ) -> ExecResult<Value> {
        self.evaluate_physical_scoped(expression, schema, row)
    }

    fn star_column_visible(&self, column: &str) -> bool {
        !matches!(column, SCORE_COLUMN | DOC_ID_COLUMN | MERGE_ACTION_COLUMN)
            && !is_score_provenance_column(column)
    }

    fn parameters(&self) -> &[SQLParam] {
        self.params
    }

    fn expression_type(
        &self,
        expression: &ScalarExpr,
        schema: &uqa_execution::RowSchema,
    ) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        uqa_execution::scalar_type_with_resolver(expression, schema, self.params, self)
    }

    fn bind_type_introspection(
        &self,
        expression: ScalarExpr,
        schema: &uqa_execution::RowSchema,
    ) -> ScalarExpr {
        uqa_execution::bind_type_introspection_with_resolver(expression, schema, self.params, self)
    }
}

impl FunctionTypeResolver for EngineExpressionEvaluator<'_> {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.engine.has_untyped_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        Ok(crate::sql::resolve_catalog_column_type(self.engine, name))
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<uqa_sql::ast::ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        self.engine.resolve_function_type(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<uqa_sql::ast::ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<uqa_execution::ResolvedFunctionOverload>, SQLError> {
        self.engine.resolve_function_overload(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn is_scalar_function_binding(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
    ) -> Result<bool, SQLError> {
        self.engine.is_scalar_function_binding(binding)
    }

    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<uqa_sql::ast::ColumnType>],
        explicit_variadic: bool,
        builtins: &[uqa_execution::BuiltinFunctionOverload],
    ) -> Result<Option<uqa_execution::ResolvedFunctionOverload>, SQLError> {
        self.engine.resolve_function_overload_with_builtins(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        )
    }

    fn resolve_scalar_subquery_type(
        &self,
        subquery: uqa_execution::SubqueryId,
        outer_schema: &uqa_execution::RowSchema,
        params: &[SQLParam],
    ) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        let plan = self.ctes.scalar_subqueries.get(subquery).ok_or_else(|| {
            SQLError::Internal(format!(
                "physical scalar subquery slot {subquery} is out of bounds"
            ))
        })?;
        let output = super::bind_query_plan_schema(
            self.engine,
            plan,
            params,
            &self.ctes,
            Some(outer_schema),
        )?;
        Ok(output.column_type(0).cloned())
    }
}

impl uqa_sql::expr::EngineHook for ScopedEngineHook<'_> {
    fn resolve_type_name(
        &self,
        name: &str,
    ) -> std::result::Result<Option<uqa_sql::ast::ColumnType>, String> {
        Ok(crate::sql::resolve_catalog_column_type(self.engine, name))
    }

    fn nextval(&self, name: &str) -> std::result::Result<i64, String> {
        self.engine.nextval(name)
    }

    fn currval(&self, name: &str) -> std::result::Result<i64, String> {
        self.engine.currval(name)
    }

    fn setval(&self, name: &str, value: i64) -> std::result::Result<i64, String> {
        self.engine.setval(name, value)
    }

    fn call_scalar_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<Value, SQLError>> {
        self.engine.call_registered_scalar_function(name, args)
    }

    fn has_scalar_functions(&self) -> bool {
        self.engine.has_registered_scalar_functions()
    }

    fn current_schema(&self) -> std::result::Result<Option<String>, String> {
        self.engine
            .current_schema_name()
            .map_err(|error| error.to_string())
    }

    fn current_schemas(
        &self,
        include_implicit: bool,
    ) -> std::result::Result<Option<Vec<String>>, String> {
        self.engine
            .current_schema_names(include_implicit)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn random_value(&self) -> std::result::Result<Option<f64>, String> {
        Ok(Some(self.engine.next_random_value()))
    }

    fn random_u64(&self) -> std::result::Result<Option<u64>, String> {
        Ok(Some(self.engine.next_random_u64()))
    }

    fn set_random_seed(&self, seed: f64) -> std::result::Result<bool, String> {
        self.engine.set_random_seed(seed)?;
        Ok(true)
    }

    fn call_user_function(
        &self,
        name: &str,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::sql::call_user_scalar_function(self.engine, name, args)
    }

    fn call_bound_user_function(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::sql::call_bound_user_scalar_function(self.engine, binding, args)
    }
}

impl PhysicalSubqueryRunner for ScopedEngineHook<'_> {
    fn execute_subquery(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<uqa_execution::SubqueryResult, SQLError> {
        let cache_key = (self.ctes.scalar_subquery_arena, subquery);
        if let Some(entry) = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned()
        {
            match entry {
                ScalarSubqueryCacheEntry::Correlated => {
                    return self.execute_correlated_subquery(plan, outer_row, params);
                }
                ScalarSubqueryCacheEntry::Materialized(result) => return result.result(),
                ScalarSubqueryCacheEntry::Scalar(_)
                | ScalarSubqueryCacheEntry::Exists(_)
                | ScalarSubqueryCacheEntry::Membership(_)
                | ScalarSubqueryCacheEntry::CorrelatedExists(_) => {
                    return Err(SQLError::Internal(
                        "scalar subquery slot changed result consumer during execution".into(),
                    ));
                }
            }
        }

        if crate::sql::correlation::query_depends_on_outer_row(self.engine, plan)? {
            self.ctes
                .scalar_subquery_cache
                .lock()
                .insert(cache_key, ScalarSubqueryCacheEntry::Correlated);
            return self.execute_correlated_subquery(plan, outer_row, params);
        }

        let result = self.execute_uncorrelated_subquery(plan, params)?;
        self.ctes.scalar_subquery_cache.lock().insert(
            cache_key,
            ScalarSubqueryCacheEntry::Materialized(result.clone()),
        );
        result.result()
    }

    fn scalar_subquery_value(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        let cache_key = (self.ctes.scalar_subquery_arena, subquery);
        if let Some(entry) = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned()
        {
            return match entry {
                ScalarSubqueryCacheEntry::Correlated => self
                    .execute_correlated_subquery(plan, outer_row, params)?
                    .into_scalar_value(),
                ScalarSubqueryCacheEntry::Scalar(value) => Ok(value),
                ScalarSubqueryCacheEntry::Materialized(result) => {
                    result.result()?.into_scalar_value()
                }
                ScalarSubqueryCacheEntry::Exists(_)
                | ScalarSubqueryCacheEntry::Membership(_)
                | ScalarSubqueryCacheEntry::CorrelatedExists(_) => Err(SQLError::Internal(
                    "scalar subquery slot changed result consumer during execution".into(),
                )),
            };
        }
        if crate::sql::correlation::query_depends_on_outer_row(self.engine, plan)? {
            self.ctes
                .scalar_subquery_cache
                .lock()
                .insert(cache_key, ScalarSubqueryCacheEntry::Correlated);
            return self
                .execute_correlated_subquery(plan, outer_row, params)?
                .into_scalar_value();
        }
        let value = self
            .execute_uncorrelated_subquery(plan, params)?
            .result()?
            .into_scalar_value()?;
        self.ctes
            .scalar_subquery_cache
            .lock()
            .insert(cache_key, ScalarSubqueryCacheEntry::Scalar(value.clone()));
        Ok(value)
    }

    fn subquery_exists(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let cache_key = (self.ctes.scalar_subquery_arena, subquery);
        if let Some(entry) = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned()
        {
            return match entry {
                ScalarSubqueryCacheEntry::Correlated => self
                    .execute_correlated_subquery(plan, outer_row, params)?
                    .into_exists(),
                ScalarSubqueryCacheEntry::CorrelatedExists(lookup) => {
                    self.correlated_exists_matches(&lookup, outer_row, params)
                }
                ScalarSubqueryCacheEntry::Exists(exists) => Ok(exists),
                ScalarSubqueryCacheEntry::Materialized(result) => Ok(result.rows.rows() != 0),
                ScalarSubqueryCacheEntry::Scalar(_) | ScalarSubqueryCacheEntry::Membership(_) => {
                    Err(SQLError::Internal(
                        "scalar subquery slot changed result consumer during execution".into(),
                    ))
                }
            };
        }
        if crate::sql::correlation::query_depends_on_outer_row(self.engine, plan)? {
            if outer_row.is_some() && !query_contains_volatile_function(self.engine, plan)? {
                if let Some(lookup) = self.build_correlated_exists(plan, params)? {
                    let exists = self.correlated_exists_matches(&lookup, outer_row, params)?;
                    self.ctes.scalar_subquery_cache.lock().insert(
                        cache_key,
                        ScalarSubqueryCacheEntry::CorrelatedExists(lookup),
                    );
                    return Ok(exists);
                }
            }
            self.ctes
                .scalar_subquery_cache
                .lock()
                .insert(cache_key, ScalarSubqueryCacheEntry::Correlated);
            return self
                .execute_correlated_subquery(plan, outer_row, params)?
                .into_exists();
        }
        let exists = self
            .execute_uncorrelated_subquery(plan, params)?
            .rows
            .rows()
            != 0;
        self.ctes
            .scalar_subquery_cache
            .lock()
            .insert(cache_key, ScalarSubqueryCacheEntry::Exists(exists));
        Ok(exists)
    }

    fn subquery_contains(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        needle: &Value,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        let cache_key = (self.ctes.scalar_subquery_arena, subquery);
        if let Some(entry) = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned()
        {
            return match entry {
                ScalarSubqueryCacheEntry::Correlated => self
                    .execute_correlated_subquery(plan, outer_row, params)?
                    .contains(needle),
                ScalarSubqueryCacheEntry::Membership(membership) => membership.contains(needle),
                ScalarSubqueryCacheEntry::Materialized(result) => {
                    let membership = Arc::new(result.membership(self.engine.work_mem_bytes()?)?);
                    let found = membership.contains(needle)?;
                    self.ctes
                        .scalar_subquery_cache
                        .lock()
                        .insert(cache_key, ScalarSubqueryCacheEntry::Membership(membership));
                    Ok(found)
                }
                ScalarSubqueryCacheEntry::Scalar(_)
                | ScalarSubqueryCacheEntry::Exists(_)
                | ScalarSubqueryCacheEntry::CorrelatedExists(_) => Err(SQLError::Internal(
                    "scalar subquery slot changed result consumer during execution".into(),
                )),
            };
        }
        if crate::sql::correlation::query_depends_on_outer_row(self.engine, plan)? {
            self.ctes
                .scalar_subquery_cache
                .lock()
                .insert(cache_key, ScalarSubqueryCacheEntry::Correlated);
            return self
                .execute_correlated_subquery(plan, outer_row, params)?
                .contains(needle);
        }

        let result = self.execute_uncorrelated_subquery(plan, params)?;
        let membership = Arc::new(result.membership(self.engine.work_mem_bytes()?)?);
        let found = membership.contains(needle)?;
        self.ctes
            .scalar_subquery_cache
            .lock()
            .insert(cache_key, ScalarSubqueryCacheEntry::Membership(membership));
        Ok(found)
    }
}

impl ScopedEngineHook<'_> {
    fn build_correlated_exists(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
    ) -> Result<Option<Arc<CachedCorrelatedExists>>, SQLError> {
        let Some(decorrelated) = crate::sql::correlation::decorrelate_exists(self.engine, plan)?
        else {
            return Ok(None);
        };
        let mut scoped_ctes = self.ctes.clone();
        scoped_ctes.lock_identities.emit = false;
        let result = execute_query_plan_output(
            self.engine,
            &decorrelated.inner,
            params,
            &mut scoped_ctes,
            QueryOutputMode::ExistsKeySet,
        )?;
        let QueryRows::ExistsKeySet(keys) = result.rows else {
            return Err(SQLError::Internal(
                "decorrelated EXISTS collector returned row output".into(),
            ));
        };
        Ok(Some(Arc::new(CachedCorrelatedExists {
            outer_keys: CorrelatedExistsOuterKeys::compile(decorrelated.outer_keys),
            keys,
        })))
    }

    fn correlated_exists_matches(
        &self,
        lookup: &CachedCorrelatedExists,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        Self::with_outer_lookup(outer_row, |outer_row| match &lookup.outer_keys {
            CorrelatedExistsOuterKeys::Direct(columns) => {
                let mut key = smallvec::SmallVec::<[&Value; 4]>::with_capacity(columns.len());
                for column in columns {
                    let Some(value) = column.value(outer_row) else {
                        return Ok(false);
                    };
                    if matches!(value, Value::Null) {
                        return Ok(false);
                    }
                    key.push(value);
                }
                lookup
                    .keys
                    .contains_borrowed(&key)
                    .map_err(physical_exec_error)
            }
            CorrelatedExistsOuterKeys::Evaluated(expressions) => {
                let context = PhysicalEvalContext::from_row_lookup(outer_row, params)
                    .with_function_hook(self)
                    .with_subquery_runner(self);
                let mut key = smallvec::SmallVec::<[Value; 4]>::with_capacity(expressions.len());
                for expression in expressions {
                    let value =
                        eval_physical_scalar(expression, &self.ctes.scalar_subqueries, &context)?;
                    if matches!(value, Value::Null) {
                        return Ok(false);
                    }
                    key.push(value);
                }
                lookup
                    .keys
                    .contains_values(&key)
                    .map_err(physical_exec_error)
            }
        })
    }

    fn execute_uncorrelated_subquery(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
    ) -> Result<CachedScalarSubquery, SQLError> {
        let mut scoped_ctes = self.ctes.clone();
        scoped_ctes.lock_identities.emit = false;
        scoped_ctes.clear_row_lock_outer_row();
        let output = execute_query_plan_output(
            self.engine,
            plan,
            params,
            &mut scoped_ctes,
            QueryOutputMode::SharedSpill,
        )?;
        let QueryRows::SharedSpill(rows) = output.rows else {
            return Err(SQLError::Internal(
                "scalar subquery spill collector returned in-memory rows".into(),
            ));
        };
        Ok(CachedScalarSubquery {
            columns: output.columns,
            rows,
        })
    }

    fn execute_correlated_subquery(
        &self,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<uqa_execution::SubqueryResult, SQLError> {
        match outer_row {
            PhysicalOuterRow::Physical { schema, row } => {
                let outer_row = uqa_execution::OwnedPhysicalRow::new(schema.clone(), row.clone());
                execute_lateral_subquery_output(self.engine, plan, &outer_row, params, self.ctes)?
                    .into_subquery_result()
            }
            PhysicalOuterRow::Absent => Err(SQLError::Internal(
                "correlated subquery reached execution without a positional outer row".into(),
            )),
        }
    }

    fn with_outer_lookup<T>(
        outer_row: PhysicalOuterRow<'_>,
        evaluate: impl FnOnce(&dyn RowLookup) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        match outer_row {
            PhysicalOuterRow::Physical { schema, row } => evaluate(&schema.view(row)),
            PhysicalOuterRow::Absent => Err(SQLError::Internal(
                "correlated subquery requires an outer row".into(),
            )),
        }
    }
}

pub(in crate::sql) fn expr_contains_subquery(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => true,
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(expr_contains_subquery),
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_contains_subquery)
                || order_by
                    .iter()
                    .any(|order| expr_contains_subquery(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|expr| expr_contains_subquery(expr))
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_contains_subquery(lhs) || expr_contains_subquery(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_contains_subquery(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_contains_subquery(expr)
                || expr_contains_subquery(low)
                || expr_contains_subquery(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_contains_subquery(expr) || list.iter().any(expr_contains_subquery)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_contains_subquery)
                || spec.partition_by.iter().any(expr_contains_subquery)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_contains_subquery(&order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_contains_subquery(&frame.start)
                        || frame_bound_contains_subquery(&frame.end)
                })
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_contains_subquery(expr))
                || when.iter().any(|(cond, result)| {
                    expr_contains_subquery(cond) || expr_contains_subquery(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_contains_subquery(expr))
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => false,
    }
}

pub(in crate::sql) fn frame_bound_contains_subquery(bound: &ScalarFrameBound) -> bool {
    match bound {
        ScalarFrameBound::Preceding(expr) | ScalarFrameBound::Following(expr) => {
            expr_contains_subquery(expr)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => false,
    }
}

#[cfg(test)]
mod tests {
    use super::CteScope;

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
}
