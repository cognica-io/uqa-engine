//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar-subquery cache values and correlation keys.

use std::sync::Arc;
use uqa_execution::RowSchemaExecution;

use uqa_core::Value;
use uqa_sql::expr::RowLookup;
use uqa_sql::SQLError;

use super::super::{
    eval_physical_scalar, execute_lateral_subquery_output, execute_query_plan_output,
    physical_exec_error, query_contains_volatile_function, PhysicalEvalContext, PhysicalOuterRow,
    PhysicalSubqueryRunner, QueryOutputMode, QueryPlan, QueryRows, SQLParam,
};
use super::callbacks::ScopedEngineHook;

pub(super) use uqa_execution::query::scope::subqueries::{
    CachedCorrelatedExists, CachedScalarSubquery, CorrelatedExistsOuterKeys,
    ScalarSubqueryCacheEntry,
};

impl PhysicalSubqueryRunner for ScopedEngineHook<'_> {
    fn execute_subquery(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<uqa_execution::SubqueryResult, SQLError> {
        let cached = self.ctes.cached_subquery(subquery);
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
                .cache_subquery(subquery, ScalarSubqueryCacheEntry::Correlated);
            return self.execute_correlated_subquery(plan, outer_row, params);
        }

        let result = self.execute_uncorrelated_subquery(plan, params)?;
        self.ctes.cache_subquery(
            subquery,
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
        let cached = self.ctes.cached_subquery(subquery);
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
                .cache_subquery(subquery, ScalarSubqueryCacheEntry::Correlated);
            return self
                .execute_correlated_subquery(plan, outer_row, params)?
                .into_scalar_value();
        }
        let value = self
            .execute_uncorrelated_subquery(plan, params)?
            .result()?
            .into_scalar_value()?;
        self.ctes
            .cache_subquery(subquery, ScalarSubqueryCacheEntry::Scalar(value.clone()));
        Ok(value)
    }

    fn subquery_exists(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let cached = self.ctes.cached_subquery(subquery);
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
                    self.ctes.cache_subquery(
                        subquery,
                        ScalarSubqueryCacheEntry::CorrelatedExists(lookup),
                    );
                    return Ok(exists);
                }
            }
            self.ctes
                .cache_subquery(subquery, ScalarSubqueryCacheEntry::Correlated);
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
            .cache_subquery(subquery, ScalarSubqueryCacheEntry::Exists(exists));
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
        let cached = self.ctes.cached_subquery(subquery);
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
                        .cache_subquery(subquery, ScalarSubqueryCacheEntry::Membership(membership));
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
                .cache_subquery(subquery, ScalarSubqueryCacheEntry::Correlated);
            return self
                .execute_correlated_subquery(plan, outer_row, params)?
                .contains(needle);
        }

        let result = self.execute_uncorrelated_subquery(plan, params)?;
        let membership = Arc::new(result.membership(self.runtime.work_mem_bytes()?)?);
        let found = membership.contains(needle)?;
        self.ctes
            .cache_subquery(subquery, ScalarSubqueryCacheEntry::Membership(membership));
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
        let mut scoped_ctes = self.ctes.as_ref().clone();
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
        let mut scoped_ctes = self.ctes.as_ref().clone();
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
                execute_lateral_subquery_output(self.engine, plan, &outer_row, params, &self.ctes)?
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
