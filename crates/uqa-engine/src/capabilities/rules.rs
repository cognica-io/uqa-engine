//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture rule namespaces and expression scopes for execution.
use crate::Engine;
use uqa_execution::{
    mutation::rules::{RuleContext, RuleSecurity, RuleStatements},
    statement::plan_executor::StatementOutput,
};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{semantics::rules::analysis::RuleAnalysisContext, SQLError};
impl Engine {
    pub(crate) fn rule_analysis_context(&self) -> RuleAnalysisContext<'_> {
        RuleAnalysisContext {
            rules: self,
            sources: self,
            returning: self,
            domains: self,
            composites: uqa_sql::expr::EngineHook::composite_types(self),
        }
    }
    pub(crate) fn rule_execution_context(
        &self,
    ) -> RuleContext<'_, crate::session::StatementReadSnapshot> {
        let runtime = self.query_runtime_view();
        RuleContext {
            memory: runtime.settings,
            cancellation: runtime.cancellation,
            analysis: self.rule_analysis_context(),
            binding: self.catalog_routine_analysis_context(),
            security: self,
            statements: self,
            returning: self.returning_analysis_context(),
            assignment: self,
            projection: None,
            volatility: self,
        }
    }
}
impl RuleSecurity for Engine {
    fn privilege_subject(&self, table: &str) -> Result<RoleReference, SQLError> {
        self.event_lookup_context().rule_privilege_subject(table)
    }
}
impl RuleStatements<crate::session::StatementReadSnapshot> for Engine {
    fn read_control(&self) -> Result<uqa_storage::read_control::StorageReadControl, SQLError> {
        self.query_retention_control()
    }
    fn execute(
        &self,
        action: uqa_sql::semantics::rules::binding::RuleAction,
        params: &[uqa_sql::SQLParam],
        inherited_ctes: Option<
            &uqa_execution::query::CteScope<crate::session::StatementReadSnapshot>,
        >,
        privilege_subject: &RoleReference,
        consumer: Option<std::rc::Rc<dyn uqa_execution::query::consumer::QueryRowConsumer + '_>>,
    ) -> Result<StatementOutput, SQLError> {
        let planning = self.statement_planning_context();
        let scopes = inherited_ctes.map(|parent| {
            uqa_sql::binding::statements::InheritedStatementAnalysisScopes {
                current: planning.analysis.scopes,
                parent,
            }
        });
        let planning = uqa_planner::statement_planning::executable::StatementPlanningContext {
            analysis: uqa_sql::binding::statements::StatementAnalysisContext {
                scopes: scopes
                    .as_ref()
                    .map_or(planning.analysis.scopes, |scope| scope),
                ..planning.analysis
            },
            ..planning
        };
        let mut compiled = self.compiled_statement_context();
        compiled.planning = &planning;
        uqa_execution::statement::compiled::execute_output_with_privilege_subject(
            &compiled,
            action,
            params,
            inherited_ctes,
            privilege_subject,
            consumer,
        )
    }
}

impl Engine {
    pub(crate) fn view_rule_execution_context(
        &self,
    ) -> uqa_execution::mutation::rules::ViewRuleContext<'_, crate::session::StatementReadSnapshot>
    {
        uqa_execution::mutation::rules::ViewRuleContext {
            rules: self.rule_execution_context(),
            views: self.view_rewrite_context(),
        }
    }
}
