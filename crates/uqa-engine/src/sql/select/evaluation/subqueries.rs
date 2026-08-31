//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar-subquery cache values and correlation keys.

use std::sync::Arc;

use uqa_core::Value;
use uqa_execution::ScalarExpr;
use uqa_sql::expr::RowLookup;
use uqa_sql::SQLError;

use super::super::{
    eval_physical_scalar, execute_lateral_subquery_output, execute_query_plan_output,
    physical_exec_error, query_contains_volatile_function, PhysicalEvalContext, PhysicalOuterRow,
    PhysicalSubqueryRunner, QueryOutputMode, QueryPlan, QueryRows, SQLParam,
};
use super::callbacks::ScopedEngineHook;

#[derive(Clone)]
pub(super) enum ScalarSubqueryCacheEntry {
    Correlated,
    CorrelatedExists(Arc<CachedCorrelatedExists>),
    Materialized(CachedScalarSubquery),
    Membership(Arc<CachedSubqueryMembership>),
    Scalar(Value),
    Exists(bool),
}

pub(super) struct CachedCorrelatedExists {
    pub(super) outer_keys: CorrelatedExistsOuterKeys,
    pub(super) keys: uqa_execution::CanonicalRowHashSet,
}

pub(super) enum CorrelatedExistsOuterKeys {
    Direct(Vec<DirectColumnKey>),
    Evaluated(Vec<ScalarExpr>),
}

impl CorrelatedExistsOuterKeys {
    pub(super) fn compile(expressions: Vec<ScalarExpr>) -> Self {
        let direct = expressions
            .iter()
            .map(DirectColumnKey::compile)
            .collect::<Option<Vec<_>>>();
        direct.map_or(Self::Evaluated(expressions), Self::Direct)
    }
}

/// A scalar key expression that can be resolved as a borrowed physical value instead of cloning it through the general expression evaluator.
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

pub(super) struct CachedSubqueryMembership {
    values: parking_lot::Mutex<uqa_execution::ExactRowSet>,
    has_column: bool,
    saw_row: bool,
    saw_null: bool,
}

impl CachedSubqueryMembership {
    pub(super) fn contains(&self, needle: &Value) -> Result<Option<bool>, SQLError> {
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
pub(super) struct CachedScalarSubquery {
    pub(super) columns: Vec<String>,
    pub(super) rows: uqa_execution::SharedSpill,
}

impl CachedScalarSubquery {
    pub(super) fn result(&self) -> Result<uqa_execution::SubqueryResult, SQLError> {
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

    pub(super) fn membership(
        &self,
        work_mem_bytes: usize,
    ) -> Result<CachedSubqueryMembership, SQLError> {
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

impl PhysicalSubqueryRunner for ScopedEngineHook<'_> {
    fn execute_subquery(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<uqa_execution::SubqueryResult, SQLError> {
        let cache_key = (self.ctes.scalar_subquery_arena, subquery);
        let cached = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned();
        if let Some(entry) = cached {
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
        let cached = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned();
        if let Some(entry) = cached {
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
        let cached = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned();
        if let Some(entry) = cached {
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
        let cached = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned();
        if let Some(entry) = cached {
            return match entry {
                ScalarSubqueryCacheEntry::Correlated => self
                    .execute_correlated_subquery(plan, outer_row, params)?
                    .contains(needle),
                ScalarSubqueryCacheEntry::Membership(membership) => membership.contains(needle),
                ScalarSubqueryCacheEntry::Materialized(result) => {
                    let membership = Arc::new(result.membership(self.runtime.work_mem_bytes()?)?);
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
        let membership = Arc::new(result.membership(self.runtime.work_mem_bytes()?)?);
        let found = membership.contains(needle)?;
        self.ctes
            .scalar_subquery_cache
            .lock()
            .insert(cache_key, ScalarSubqueryCacheEntry::Membership(membership));
        Ok(found)
    }
}

impl ScopedEngineHook<'_> {
    pub(super) fn build_correlated_exists(
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

    pub(super) fn correlated_exists_matches(
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
