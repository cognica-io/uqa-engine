//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture a statement read scope and borrow its native query and expression inputs.

use crate::{
    query::{locking::RowLockContext, statement::context::QueryContext, CteScope},
    scalar::plan::PhysicalEvalContext,
};
use uqa_sql::{SQLError, SQLParam, SQLResult};
pub type StatementExpressionOperation<'a> =
    dyn FnMut(&PhysicalEvalContext<'_>) -> Result<SQLResult, SQLError> + 'a;
pub trait StatementQueryContexts<S: Clone + 'static> {
    fn statement_scope(&self, privilege_subject: Option<&str>) -> CteScope<S>;
    fn query_context(&self) -> QueryContext<'_, S>;
    fn row_lock_context(&self) -> RowLockContext<'_, S>;
    fn with_expression_context(
        &self,
        scope: &CteScope<S>,
        params: &[SQLParam],
        operation: &mut StatementExpressionOperation<'_>,
    ) -> Result<SQLResult, SQLError>;
}
