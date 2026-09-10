//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query read generation, physical sources, and directional execution services.

use crate::query::{sources::SourceContext, CteScope};
use crate::{PhysicalOperator, RowSchema};
use std::collections::BTreeMap;
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam, ScalarExpr};

/// Select a read generation while keeping its owner alive only for the borrowed operation.
pub trait SnapshotSource<S: Clone + 'static> {
    fn capture(&self) -> Result<S, SQLError>;
}

pub trait QuerySnapshots<S: Clone + 'static>: SnapshotSource<S> {
    fn with_snapshot(
        &self,
        snapshot: &S,
        operation: &mut dyn ScopedQueryOperation<S>,
    ) -> Result<(), SQLError>;
}
pub trait ScopedQueryOperation<S: Clone + 'static> {
    fn run(&mut self, context: &QueryContext<'_, S>) -> Result<(), SQLError>;
}
/// Fork session state for a directional child. Physical traversal belongs to execution.
pub trait DirectionalQueryFactory<S: Clone + 'static> {
    fn query_operator(
        &self,
        plan: QueryPlan,
        params: Vec<SQLParam>,
        scope: CteScope<S>,
        schema: RowSchema,
    ) -> Result<Box<dyn PhysicalOperator>, SQLError>;
}
pub trait CteFilterPlanning<S: Clone> {
    fn output_filters(
        &self,
        plan: &QueryPlan,
        scope: &CteScope<S>,
    ) -> Result<BTreeMap<String, (String, ScalarExpr)>, SQLError>;
}
#[derive(Clone)]
pub struct QueryContext<'a, S: Clone + 'static> {
    pub generation: Option<&'a S>,
    pub source: SourceContext<'a, S>,
    pub snapshots: &'a dyn QuerySnapshots<S>,
    pub directional: &'a dyn DirectionalQueryFactory<S>,
    pub cte_filters: &'a dyn CteFilterPlanning<S>,
}
impl<S: Clone + 'static> Copy for QueryContext<'_, S> {}

pub fn with_query_snapshot<S: Clone + 'static, T>(
    snapshots: &dyn QuerySnapshots<S>,
    snapshot: &S,
    action: impl for<'scope> FnOnce(&QueryContext<'scope, S>) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    struct Operation<F, T> {
        action: Option<F>,
        result: Option<T>,
    }
    impl<S: Clone + 'static, F, T> ScopedQueryOperation<S> for Operation<F, T>
    where
        F: for<'scope> FnOnce(&QueryContext<'scope, S>) -> Result<T, SQLError>,
    {
        fn run(&mut self, context: &QueryContext<'_, S>) -> Result<(), SQLError> {
            let action = self.action.take().ok_or_else(|| {
                SQLError::Internal("statement snapshot operation was invoked more than once".into())
            })?;
            self.result = Some(action(context)?);
            Ok(())
        }
    }
    let mut operation = Operation {
        action: Some(action),
        result: None,
    };
    snapshots.with_snapshot(snapshot, &mut operation)?;
    operation
        .result
        .ok_or_else(|| SQLError::Internal("statement snapshot operation was not invoked".into()))
}
