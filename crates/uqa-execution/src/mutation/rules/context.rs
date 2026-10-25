//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capabilities required to execute stored rewrite rules.
use crate::{
    query::runtime::QueryMemorySettings, statement::plan_executor::StatementOutput, PhysicalRow,
    RowSchema,
};
use uqa_core::Value;
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    assignment::AssignmentContext,
    plan::ExpressionPlan,
    semantics::{returning::ReturningAnalysisContext, rules::analysis::RuleAnalysisContext},
    SQLError,
};
pub trait RuleSecurity {
    fn privilege_subject(&self, table: &str) -> Result<RoleReference, SQLError>;
}
pub trait RuleStatements<S: Clone + 'static> {
    fn execute(
        &self,
        action: uqa_sql::semantics::rules::binding::RuleAction,
        params: &[uqa_sql::SQLParam],
        inherited_ctes: Option<&crate::query::CteScope<S>>,
        privilege_subject: &RoleReference,
        consumer: Option<std::rc::Rc<dyn crate::query::consumer::QueryRowConsumer + '_>>,
    ) -> Result<StatementOutput, SQLError>;
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
#[derive(Clone)]
pub struct RuleContext<'a, S: Clone + 'static> {
    pub memory: &'a dyn QueryMemorySettings,
    pub cancellation: &'a uqa_core::CancellationToken,
    pub analysis: RuleAnalysisContext<'a>,
    pub binding: uqa_sql::binding::stored_routines::analysis::CatalogRoutineAnalysisContext<'a>,
    pub security: &'a dyn RuleSecurity,
    pub statements: &'a dyn RuleStatements<S>,
    pub expressions: &'a dyn RuleExpressions,
    pub returning: ReturningAnalysisContext<'a>,
    pub assignment: &'a dyn AssignmentContext,
    pub projection: Option<&'a dyn super::RuleReturningProjection>,
    pub volatility: &'a dyn uqa_sql::semantics::volatility::VolatilityCatalog,
}

impl<S: Clone + 'static> Copy for RuleContext<'_, S> {}

impl<'a, S: Clone + 'static> RuleContext<'a, S> {
    pub fn with_projection<'b>(
        self,
        projection: &'b dyn super::RuleReturningProjection,
    ) -> RuleContext<'b, S>
    where
        'a: 'b,
    {
        RuleContext {
            projection: Some(projection),
            ..self
        }
    }
}
