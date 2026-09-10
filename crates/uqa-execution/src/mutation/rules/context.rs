//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capabilities required to execute stored rewrite rules.
use crate::{PhysicalRow, RowSchema};
use uqa_core::Value;
use uqa_sql::{
    assignment::AssignmentContext,
    ast::{Expr, Statement},
    plan::ExpressionPlan,
    semantics::{returning::ReturningAnalysisContext, rules::analysis::RuleAnalysisContext},
    SQLError, SQLResult,
};
pub trait RuleSecurity {
    fn privilege_subject(&self, table: &str) -> Result<String, SQLError>;
}
pub trait RuleStatements {
    fn execute(&self, statement: Statement, privilege_subject: &str)
        -> Result<SQLResult, SQLError>;
}
pub trait RuleExpressions {
    fn evaluate(&self, expression: &Expr) -> Result<Value, SQLError>;
    fn evaluate_stored(
        &self,
        expression: &ExpressionPlan,
        schema: &RowSchema,
        row: &PhysicalRow,
        privilege_subject: &str,
    ) -> Result<Value, SQLError>;
}
#[derive(Clone, Copy)]
pub struct RuleContext<'a> {
    pub analysis: RuleAnalysisContext<'a>,
    pub security: &'a dyn RuleSecurity,
    pub statements: &'a dyn RuleStatements,
    pub expressions: &'a dyn RuleExpressions,
    pub returning: ReturningAnalysisContext<'a>,
    pub assignment: &'a dyn AssignmentContext,
}
