//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capabilities required to execute stored rewrite rules.
use crate::{query::runtime::QueryMemorySettings, statement::plan_executor::StatementOutput};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    assignment::AssignmentContext,
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
#[derive(Clone)]
pub struct RuleContext<'a, S: Clone + 'static> {
    pub memory: &'a dyn QueryMemorySettings,
    pub cancellation: &'a uqa_core::CancellationToken,
    pub analysis: RuleAnalysisContext<'a>,
    pub binding: uqa_sql::binding::stored_routines::analysis::CatalogRoutineAnalysisContext<'a>,
    pub security: &'a dyn RuleSecurity,
    pub statements: &'a dyn RuleStatements<S>,
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

#[derive(Clone)]
pub struct ViewRuleContext<'a, S: Clone + 'static> {
    pub rules: RuleContext<'a, S>,
    pub views: uqa_sql::semantics::view_rewrite::context::ViewRewriteContext<'a>,
}
impl<S: Clone + 'static> Copy for ViewRuleContext<'_, S> {}

impl<S: Clone + 'static> RuleContext<'_, S> {
    pub(super) fn execute_action(
        &self,
        statement: uqa_sql::ast::Statement,
        input: Option<uqa_sql::semantics::rules::binding::RuleQueryInput>,
        params: &[uqa_sql::SQLParam],
        scope: &crate::query::CteScope<S>,
        subject: &RoleReference,
        consumer: Option<std::rc::Rc<dyn crate::query::consumer::QueryRowConsumer + '_>>,
    ) -> Result<StatementOutput, SQLError> {
        let _stored =
            uqa_sql::semantics::sets::placement_scope::SetPlacementScope::stored_definition();
        self.statements.execute(
            uqa_sql::semantics::rules::binding::RuleAction::bind(
                self.analysis,
                self.returning.aggregates,
                statement,
                input,
            )?,
            params,
            Some(scope),
            subject,
            consumer,
        )
    }

    pub(super) fn source_schema(
        &self,
        input: uqa_sql::semantics::rules::binding::RuleMutation<'_>,
        params: &[uqa_sql::SQLParam],
        scope: &crate::query::CteScope<S>,
    ) -> Result<Option<crate::RowSchema>, SQLError> {
        let binding = crate::query::binding::binding_context(scope)?;
        input
            .source()
            .map(|source| {
                uqa_sql::binding::analyze_source_plan_schema(
                    self.returning.routines,
                    source,
                    params,
                    &binding,
                    None,
                )
            })
            .transpose()
    }

    pub(super) fn mutation_input_schemas(
        &self,
        input: uqa_sql::semantics::rules::binding::RuleMutation<'_>,
        params: &[uqa_sql::SQLParam],
        binding: &uqa_sql::binding::BindingContext<'_>,
    ) -> Result<(crate::RowSchema, Option<crate::RowSchema>), SQLError> {
        let analyze = |source: &uqa_sql::plan::SourcePlan| {
            uqa_sql::binding::analyze_source_plan_schema(
                self.returning.routines,
                source,
                params,
                binding,
                None,
            )
        };
        Ok((
            analyze(&input.target_source())?,
            input.source().map(analyze).transpose()?,
        ))
    }
}
