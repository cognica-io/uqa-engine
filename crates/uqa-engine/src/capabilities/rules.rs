//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture rule namespaces and expression scopes for execution.
use crate::Engine;
use uqa_core::Value;
use uqa_execution::{
    mutation::rules::{RuleContext, RuleExpressions, RuleSecurity, RuleStatements},
    PhysicalRow, RowSchema,
};
use uqa_sql::{
    ast::{Expr, Statement},
    plan::ExpressionPlan,
    semantics::rules::analysis::RuleAnalysisContext,
    SQLError, SQLResult,
};
impl Engine {
    pub(crate) fn rule_analysis_context(&self) -> RuleAnalysisContext<'_> {
        RuleAnalysisContext {
            rules: self,
            sources: self,
            returning: self,
        }
    }
    pub(crate) fn rule_execution_context(&self) -> RuleContext<'_> {
        RuleContext {
            analysis: self.rule_analysis_context(),
            security: self,
            statements: self,
            expressions: self,
            returning: self.returning_analysis_context(),
            assignment: self,
        }
    }
}
impl RuleSecurity for Engine {
    fn privilege_subject(&self, table: &str) -> Result<String, SQLError> {
        self.event_lookup_context().rule_privilege_subject(table)
    }
}
impl RuleStatements for Engine {
    fn execute(
        &self,
        statement: Statement,
        privilege_subject: &str,
    ) -> Result<SQLResult, SQLError> {
        uqa_execution::statement::compiled::execute_with_privilege_subject(
            &self.compiled_statement_context(),
            statement,
            &[],
            privilege_subject,
        )
    }
}
impl RuleExpressions for Engine {
    fn evaluate(&self, expression: &Expr) -> Result<Value, SQLError> {
        crate::capabilities::query_expressions::eval_lowered_expression(self, expression, None, &[])
    }
    fn evaluate_stored(
        &self,
        expression: &ExpressionPlan,
        schema: &RowSchema,
        row: &PhysicalRow,
        privilege_subject: &str,
    ) -> Result<Value, SQLError> {
        crate::capabilities::query_expressions::eval_stored_expression_plan_with_row(
            self,
            expression,
            schema,
            row,
            &[],
            Some(privilege_subject),
        )
    }
}

impl Engine {
    pub(crate) fn view_rule_execution_context(
        &self,
    ) -> uqa_execution::mutation::rules::views::ViewRuleContext<
        '_,
        crate::session::StatementReadSnapshot,
    > {
        uqa_execution::mutation::rules::views::ViewRuleContext {
            rules: self.rule_execution_context(),
            views: self.view_row_context(),
        }
    }
}
