//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute scalar subqueries with scope-local caches and correlated key probes.

use super::scope::subqueries::ScalarSubqueryCacheEntry;
use crate::scalar::plan::{PhysicalOuterRow, PhysicalSubqueryRunner};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{
    plan::QueryPlan, semantics::volatility::query_contains_volatile_function, SQLError, SQLParam,
};

mod analysis;
pub mod context;
mod execution;
mod probe;
pub use context::{SubqueryContext, SubqueryServices};
pub use probe::prepare_correlated_exists_predicate;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

impl<S: Clone + Send + Sync + 'static> PhysicalSubqueryRunner for SubqueryContext<'_, S> {
    fn execute_subquery(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<crate::SubqueryResult, SQLError> {
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

        if analysis::query_depends_on_outer_row(&self.services, plan)? {
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
        if analysis::query_depends_on_outer_row(&self.services, plan)? {
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
        if analysis::query_depends_on_outer_row(&self.services, plan)? {
            if outer_row.is_some()
                && !query_contains_volatile_function(self.services.volatility, plan)?
            {
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
                    let membership = Arc::new(result.membership(self.memory.work_mem_bytes()?)?);
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
        if analysis::query_depends_on_outer_row(&self.services, plan)? {
            self.ctes
                .cache_subquery(subquery, ScalarSubqueryCacheEntry::Correlated);
            return self
                .execute_correlated_subquery(plan, outer_row, params)?
                .contains(needle);
        }

        let result = self.execute_uncorrelated_subquery(plan, params)?;
        let membership = Arc::new(result.membership(self.memory.work_mem_bytes()?)?);
        let found = membership.contains(needle)?;
        self.ctes
            .cache_subquery(subquery, ScalarSubqueryCacheEntry::Membership(membership));
        Ok(found)
    }
}
