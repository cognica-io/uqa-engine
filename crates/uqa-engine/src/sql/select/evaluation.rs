//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped scalar, function, and subquery evaluation.

use super::{
    engine_func_intercept, eval_physical_scalar, execute_lateral_subquery_output,
    execute_query_plan_output, is_score_provenance_column, physical_exec_error, Arc, AtomicU64,
    BTreeMap, Engine, ExecResult, ExpressionEvaluator, Ordering, PhysicalEvalContext,
    PhysicalSubqueryRunner, QueryOutputMode, QueryPlan, QueryRows, ResultRow, SQLError, SQLParam,
    ScalarExpr, ScalarFrameBound, SharedExpressionEvaluator, Value, DOC_ID_COLUMN,
    MERGE_ACTION_COLUMN, SCORE_COLUMN,
};

#[derive(Clone)]
pub(crate) struct CteScope {
    pub(in crate::sql) rows: BTreeMap<String, uqa_execution::SharedSpill>,
    pub(in crate::sql) scalar_subqueries: Vec<QueryPlan>,
    scalar_subquery_arena: u64,
    next_scalar_subquery_arena: Arc<AtomicU64>,
    scalar_subquery_cache:
        Arc<parking_lot::Mutex<BTreeMap<(u64, usize), ScalarSubqueryCacheEntry>>>,
}

impl Default for CteScope {
    fn default() -> Self {
        Self {
            rows: BTreeMap::new(),
            scalar_subqueries: Vec::new(),
            scalar_subquery_arena: 0,
            next_scalar_subquery_arena: Arc::new(AtomicU64::new(1)),
            scalar_subquery_cache: Arc::new(parking_lot::Mutex::new(BTreeMap::new())),
        }
    }
}

impl CteScope {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(in crate::sql) fn insert_shared(&mut self, name: String, rows: uqa_execution::SharedSpill) {
        self.rows.insert(name, rows);
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
        ScalarSubqueryScope {
            ctes: self,
            previous: Some(previous),
            previous_arena,
        }
    }
}

pub(in crate::sql) struct ScalarSubqueryScope<'a> {
    ctes: &'a mut CteScope,
    previous: Option<Vec<QueryPlan>>,
    previous_arena: u64,
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
        }
    }
}

#[derive(Clone)]
pub(in crate::sql) enum ScalarSubqueryCacheEntry {
    Correlated,
    Materialized(CachedScalarSubquery),
    Scalar(Value),
    Exists(bool),
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
            .map(|row| {
                let mut row = row.map_err(physical_exec_error)?;
                row.retain(|column, _| !is_score_provenance_column(column));
                Ok(row)
            });
        Ok(uqa_execution::SubqueryResult {
            columns: self.columns.clone(),
            rows: Box::new(rows),
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
}

impl ExpressionEvaluator for EngineExpressionEvaluator<'_> {
    fn evaluate(&self, expression: &ScalarExpr, row: &ResultRow) -> ExecResult<Value> {
        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let context = PhysicalEvalContext::new(Some(row), self.params)
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

    fn project_star(&self, row: &ResultRow) -> ExecResult<ResultRow> {
        Ok(row
            .iter()
            .filter(|(column, _)| {
                !matches!(
                    column.as_str(),
                    SCORE_COLUMN | DOC_ID_COLUMN | MERGE_ACTION_COLUMN
                ) && !is_score_provenance_column(column)
            })
            .map(|(column, value)| (column.clone(), value.clone()))
            .collect())
    }
}

impl uqa_sql::expr::EngineHook for ScopedEngineHook<'_> {
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
}

impl PhysicalSubqueryRunner for ScopedEngineHook<'_> {
    fn execute_subquery(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: Option<&ResultRow>,
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
                ScalarSubqueryCacheEntry::Scalar(_) | ScalarSubqueryCacheEntry::Exists(_) => {
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
        outer_row: Option<&ResultRow>,
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
                ScalarSubqueryCacheEntry::Exists(_) => Err(SQLError::Internal(
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
        outer_row: Option<&ResultRow>,
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
                ScalarSubqueryCacheEntry::Exists(exists) => Ok(exists),
                ScalarSubqueryCacheEntry::Materialized(result) => Ok(result.rows.rows() != 0),
                ScalarSubqueryCacheEntry::Scalar(_) => Err(SQLError::Internal(
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
}

impl ScopedEngineHook<'_> {
    fn execute_uncorrelated_subquery(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
    ) -> Result<CachedScalarSubquery, SQLError> {
        let mut scoped_ctes = self.ctes.clone();
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
        outer_row: Option<&ResultRow>,
        params: &[SQLParam],
    ) -> Result<uqa_execution::SubqueryResult, SQLError> {
        if let Some(outer_row) = outer_row {
            return execute_lateral_subquery_output(
                self.engine,
                plan,
                outer_row,
                params,
                self.ctes,
            )?
            .into_subquery_result();
        }
        let mut scoped_ctes = self.ctes.clone();
        execute_query_plan_output(
            self.engine,
            plan,
            params,
            &mut scoped_ctes,
            QueryOutputMode::SharedSpill,
        )?
        .into_subquery_result()
    }
}

pub(in crate::sql) fn expr_contains_subquery(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_contains_subquery)
        }
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
        ScalarExpr::Star
        | ScalarExpr::Column(_)
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
