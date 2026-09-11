//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize uncached rows and decorrelated keys through native query execution.

use super::{analysis, SubqueryContext, SubqueryServices};
use crate::query::scope::subqueries::{
    CachedCorrelatedExists, CachedScalarSubquery, CorrelatedExistsOuterKeys,
};
use crate::query::{
    output::QueryRows,
    sources::lateral_query::execute_lateral_subquery_output,
    statement::{consumer::QueryOutputMode, execute_query_plan_output},
    CteScope,
};
use crate::scalar::plan::PhysicalOuterRow;
use std::sync::Arc;
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam};

pub(super) fn build_correlated_exists<S: Clone + Send + Sync + 'static>(
    services: &SubqueryServices<'_, S>,
    ctes: &CteScope<S>,
    plan: &QueryPlan,
    params: &[SQLParam],
) -> Result<Option<Arc<CachedCorrelatedExists>>, SQLError> {
    let Some(decorrelated) = analysis::decorrelate_exists(services, plan)? else {
        return Ok(None);
    };
    let mut scoped_ctes = ctes.clone();
    scoped_ctes.lock_identities.emit = false;
    let result = execute_query_plan_output(
        &services.queries.query_context(),
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

impl<S: Clone + Send + Sync + 'static> SubqueryContext<'_, S> {
    pub(super) fn build_correlated_exists(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
    ) -> Result<Option<Arc<CachedCorrelatedExists>>, SQLError> {
        build_correlated_exists(&self.services, self.ctes, plan, params)
    }

    pub(super) fn execute_uncorrelated_subquery(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
    ) -> Result<CachedScalarSubquery, SQLError> {
        let mut scoped_ctes = self.ctes.clone();
        scoped_ctes.lock_identities.emit = false;
        scoped_ctes.clear_row_lock_outer_row();
        let output = execute_query_plan_output(
            &self.services.queries.query_context(),
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

    pub(super) fn execute_correlated_subquery(
        &self,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<crate::SubqueryResult, SQLError> {
        match outer_row {
            PhysicalOuterRow::Physical { schema, row } => {
                let outer_row = crate::OwnedPhysicalRow::new(schema.clone(), row.clone());
                execute_lateral_subquery_output(
                    &self.services.queries.source_context(),
                    plan,
                    &outer_row,
                    params,
                    self.ctes,
                )?
                .into_subquery_result()
            }
            PhysicalOuterRow::Absent => Err(SQLError::Internal(
                "correlated subquery reached execution without a positional outer row".into(),
            )),
        }
    }
}
