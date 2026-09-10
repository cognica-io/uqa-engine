//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capabilities bound to one statement generation. Query output may feed an INSERT consumer in the same generation.

use crate::mutation::insert::source::InsertSourceContext;
use crate::query::{sources::SourceContext, CteScope};
use crate::{PhysicalOperator, RowSchema};
use std::collections::BTreeMap;
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam, ScalarExpr};

/// Select a read generation while keeping its owner alive only for the borrowed operation.
pub trait StatementSnapshots<S: Clone + 'static> {
    fn capture(&self) -> Result<S, SQLError>;
    fn with_snapshot(
        &self,
        snapshot: &S,
        operation: &mut dyn ScopedStatementOperation<S>,
    ) -> Result<(), SQLError>;
}
pub trait ScopedStatementOperation<S: Clone + 'static> {
    fn run(&mut self, context: &StatementContext<'_, S>) -> Result<(), SQLError>;
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
pub struct StatementContext<'a, S: Clone + 'static> {
    pub source: SourceContext<'a, S>,
    pub mutation: crate::mutation::statement::MutationExecutionContext<'a, S>,
    pub snapshots: &'a dyn StatementSnapshots<S>,
    pub directional: &'a dyn DirectionalQueryFactory<S>,
    pub cte_filters: &'a dyn CteFilterPlanning<S>,
}
impl<S: Clone + 'static> Copy for StatementContext<'_, S> {}

impl<'a, S: Clone + 'static> StatementContext<'a, S> {
    pub fn insert_source(&self) -> InsertSourceContext<'a, S> {
        InsertSourceContext {
            rows: self.mutation.preparation,
            identities: self.mutation.identities,
            runtime: self.source.relational.runtime,
        }
    }
}

pub fn with_statement_snapshot<S: Clone + 'static, T>(
    snapshots: &dyn StatementSnapshots<S>,
    snapshot: &S,
    action: impl for<'scope> FnOnce(&StatementContext<'scope, S>) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    struct Operation<F, T> {
        action: Option<F>,
        result: Option<T>,
    }
    impl<S: Clone + 'static, F, T> ScopedStatementOperation<S> for Operation<F, T>
    where
        F: for<'scope> FnOnce(&StatementContext<'scope, S>) -> Result<T, SQLError>,
    {
        fn run(&mut self, context: &StatementContext<'_, S>) -> Result<(), SQLError> {
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
