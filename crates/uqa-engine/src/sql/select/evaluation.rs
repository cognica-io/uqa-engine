//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped scalar, function, and subquery evaluation.

use super::{
    engine_func_intercept, eval_physical_scalar, execute_lateral_subquery_output,
    execute_query_plan_output, is_score_provenance_column, physical_exec_error,
    query_contains_volatile_function, Arc, AtomicU64, BTreeMap, Engine, ExecResult,
    ExpressionEvaluator, Ordering, PhysicalEvalContext, PhysicalSubqueryRunner, QueryOutputMode,
    QueryPlan, QueryRows, ResultRow, SQLError, SQLParam, ScalarExpr, ScalarFrameBound,
    SharedExpressionEvaluator, Value, DOC_ID_COLUMN, MERGE_ACTION_COLUMN, SCORE_COLUMN,
};
use uqa_sql::expr::RowLookup;

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
    Qualified {
        qualifier: String,
        column: String,
        key: String,
    },
}

impl DirectColumnKey {
    pub(in crate::sql) fn compile(expression: &ScalarExpr) -> Option<Self> {
        match expression {
            ScalarExpr::Column(column) => Some(Self::Column(column.clone())),
            ScalarExpr::QualifiedColumn {
                qualifier,
                column,
                key,
            } => Some(Self::Qualified {
                qualifier: qualifier.clone(),
                column: column.clone(),
                key: key.clone(),
            }),
            _ => None,
        }
    }

    pub(in crate::sql) fn value<'a>(&self, row: &'a dyn RowLookup) -> Option<&'a Value> {
        match self {
            Self::Column(column) => row.column(column),
            Self::Qualified {
                qualifier,
                column,
                key,
            } => row.qualified_column(qualifier, column, key),
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
    fn keep(&self, row: &dyn RowLookup) -> ExecResult<bool> {
        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let exists = hook.correlated_exists_matches(&self.lookup, Some(row), self.params)?;
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

    fn project_star(&self, row: &dyn RowLookup) -> ExecResult<ResultRow> {
        let mut output = ResultRow::new();
        row.visit_columns(&mut |column, value| {
            if !matches!(column, SCORE_COLUMN | DOC_ID_COLUMN | MERGE_ACTION_COLUMN)
                && !is_score_provenance_column(column)
            {
                output.insert(column.to_string(), value.clone());
            }
        });
        Ok(output)
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
        outer_row: Option<&dyn RowLookup>,
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
        outer_row: Option<&dyn RowLookup>,
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
        outer_row: Option<&dyn RowLookup>,
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
        outer_row: Option<&dyn RowLookup>,
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
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let outer_row = outer_row.ok_or_else(|| {
            SQLError::Internal("correlated EXISTS lookup requires an outer row".into())
        })?;
        match &lookup.outer_keys {
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
        }
    }

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
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<uqa_execution::SubqueryResult, SQLError> {
        if let Some(outer_row) = outer_row {
            let mut named = ResultRow::new();
            outer_row.visit_lookup_columns(&mut |column, value| {
                named.insert(column.to_string(), value.clone());
            });
            return execute_lateral_subquery_output(self.engine, plan, &named, params, self.ctes)?
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
