//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped expression and assignment services for prepared SQL arguments.

use crate::scalar::plan::PhysicalEvalContext;
use uqa_sql::{
    assignment::AssignmentContext, plan::ExpressionPlan,
    prepared::arguments::ArgumentValidationContext, ColumnType, SQLError, SQLParam,
};

pub struct ArgumentBindingContext<'a> {
    pub validation: ArgumentValidationContext<'a>,
    pub assignment: &'a dyn AssignmentContext,
    pub analyze_type: &'a mut dyn FnMut(&ExpressionPlan) -> Result<Option<ColumnType>, SQLError>,
    pub evaluation: PhysicalEvalContext<'a>,
}

pub type ScopedArgumentOperation<'a> =
    dyn for<'scope> FnMut(ArgumentBindingContext<'scope>) -> Result<Vec<SQLParam>, SQLError> + 'a;

/// Capture the routine and statement scope only after definition and arity validation succeed.
pub trait PreparedArgumentScopes {
    fn with_scope(
        &self,
        parameters: &[SQLParam],
        operation: &mut ScopedArgumentOperation<'_>,
    ) -> Result<Vec<SQLParam>, SQLError>;
}
