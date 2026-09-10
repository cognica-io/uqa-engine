//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read generation and writable services for mutation command orchestration.
use super::MutationExecutionContext;
use crate::mutation::insert::source::InsertSourceContext;
use crate::query::statement::context::{QueryContext, SnapshotSource};
use uqa_sql::SQLError;

pub trait MutationSnapshots<S: Clone + 'static>: SnapshotSource<S> {
    fn with_snapshot(
        &self,
        snapshot: &S,
        operation: &mut dyn ScopedMutationOperation<S>,
    ) -> Result<(), SQLError>;
}
pub trait ScopedMutationOperation<S: Clone + 'static> {
    fn run(&mut self, context: &MutationStatementContext<'_, S>) -> Result<(), SQLError>;
}
#[derive(Clone)]
pub struct MutationStatementContext<'a, S: Clone + 'static> {
    pub query: QueryContext<'a, S>,
    pub mutation: MutationExecutionContext<'a, S>,
    pub snapshots: &'a dyn MutationSnapshots<S>,
}
impl<S: Clone + 'static> Copy for MutationStatementContext<'_, S> {}

impl<'a, S: Clone + 'static> MutationStatementContext<'a, S> {
    pub fn insert_source(&self) -> InsertSourceContext<'a, S> {
        InsertSourceContext {
            rows: self.mutation.preparation,
            identities: self.mutation.identities,
            runtime: self.query.source.relational.runtime,
        }
    }
}

pub fn with_mutation_snapshot<S: Clone + 'static, T>(
    snapshots: &dyn MutationSnapshots<S>,
    snapshot: &S,
    action: impl for<'scope> FnOnce(&MutationStatementContext<'scope, S>) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    struct Operation<F, T> {
        action: Option<F>,
        result: Option<T>,
    }
    impl<S: Clone + 'static, F, T> ScopedMutationOperation<S> for Operation<F, T>
    where
        F: for<'scope> FnOnce(&MutationStatementContext<'scope, S>) -> Result<T, SQLError>,
    {
        fn run(&mut self, context: &MutationStatementContext<'_, S>) -> Result<(), SQLError> {
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
