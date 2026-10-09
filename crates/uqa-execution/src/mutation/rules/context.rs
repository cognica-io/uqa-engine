//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capabilities required to execute stored rewrite rules.
use crate::{PhysicalRow, RowSchema};
use uqa_core::Value;
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    assignment::AssignmentContext,
    ast::Statement,
    plan::ExpressionPlan,
    semantics::{returning::ReturningAnalysisContext, rules::analysis::RuleAnalysisContext},
    SQLError, SQLResult,
};
pub trait RuleSecurity {
    fn privilege_subject(&self, table: &str) -> Result<RoleReference, SQLError>;
}
pub trait RuleStatements {
    fn execute(
        &self,
        statement: Statement,
        privilege_subject: &RoleReference,
    ) -> Result<SQLResult, SQLError>;
}
pub trait RuleExpressions {
    fn prepare_condition(&self, expression: &mut ExpressionPlan) -> Result<(), SQLError>;
    fn evaluate_stored(
        &self,
        expression: &ExpressionPlan,
        schema: &RowSchema,
        row: &PhysicalRow,
        privilege_subject: &RoleReference,
    ) -> Result<Value, SQLError>;
}
#[derive(Clone, Copy)]
pub struct RuleContext<'a> {
    pub analysis: RuleAnalysisContext<'a>,
    pub binding: uqa_sql::binding::stored_routines::analysis::CatalogRoutineAnalysisContext<'a>,
    pub security: &'a dyn RuleSecurity,
    pub statements: &'a dyn RuleStatements,
    pub expressions: &'a dyn RuleExpressions,
    pub returning: ReturningAnalysisContext<'a>,
    pub assignment: &'a dyn AssignmentContext,
}
