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
    fn read_control(&self) -> Result<uqa_storage::read_control::StorageReadControl, SQLError>;
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
    pub(crate) fn bind_insert_queries(
        &self,
        insert: &uqa_sql::plan::InsertPlan,
        subject: &RoleReference,
    ) -> Result<uqa_sql::semantics::rules::binding::OwnedInsertRuleQueries, SQLError> {
        let retained = self.statements.read_control()?;
        let control = uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.cancellation,
        );
        uqa_sql::semantics::rules::binding::OwnedInsertRuleQueries::bind(
            self.analysis,
            insert,
            &|table| self.security.privilege_subject(table),
            subject,
            &control,
        )
    }

    pub(super) fn returning_columns(
        &self,
        table: &str,
    ) -> Result<uqa_core::memory::Produced<Vec<uqa_sql::ast::ColumnDef>>, SQLError> {
        let retained = self.statements.read_control()?;
        let control = uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.cancellation,
        );
        uqa_sql::semantics::rules::analysis::rule_returning_columns_with_control(
            self.analysis,
            table,
            &control,
        )
    }

    pub(crate) fn capture_rules(
        &self,
        table: &str,
        event: uqa_sql::ast::RuleEvent,
    ) -> Result<uqa_sql::catalog::events::selection::CapturedRules, SQLError> {
        let retained = self.statements.read_control()?;
        let control = uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.cancellation,
        );
        self.analysis
            .rules
            .capture_rules_with_control(table, event, &control)
    }

    pub(super) fn returning_request(
        &self,
        returning: &[uqa_sql::plan::ProjectionPlan],
        aliases: &uqa_sql::ast::ReturningAliases,
        subqueries: &[uqa_sql::plan::QueryPlan],
    ) -> Result<super::RuleReturningRequest, SQLError> {
        let retained = self.statements.read_control()?;
        let control = uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.cancellation,
        );
        super::RuleReturningRequest::from_plan_with_control(
            returning, aliases, subqueries, &control,
        )
    }

    pub(super) fn execute_bound_action(
        &self,
        statement: &uqa_sql::semantics::rules::action_binding::BoundRuleStatement,
        input: Option<uqa_sql::semantics::rules::binding::OwnedRuleQueryInput>,
        params: &[uqa_sql::SQLParam],
        scope: &crate::query::CteScope<S>,
        subject: &RoleReference,
        consumer: Option<std::rc::Rc<dyn crate::query::consumer::QueryRowConsumer + '_>>,
    ) -> Result<StatementOutput, SQLError> {
        let _stored =
            uqa_sql::semantics::sets::placement_scope::SetPlacementScope::stored_definition();
        let retained = self.statements.read_control()?;
        let control = uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.cancellation,
        );
        self.statements.execute(
            uqa_sql::semantics::rules::binding::RuleAction::bind_with_control(
                self.analysis,
                self.returning.aggregates,
                statement,
                input,
                &control,
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
        let retained = self.statements.read_control()?;
        let control = uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.cancellation,
        );
        let binding = crate::query::binding::binding_context_with_control(scope, &control)?;
        input
            .source()
            .map(|source| {
                uqa_sql::binding::production::analyze_source_plan_schema_with_control(
                    self.returning.routines,
                    source,
                    params,
                    binding,
                    None,
                    &control,
                )
            })
            .transpose()
    }

    pub(super) fn mutation_input_schemas(
        &self,
        input: uqa_sql::semantics::rules::binding::RuleMutation<'_>,
        params: &[uqa_sql::SQLParam],
        binding: &uqa_sql::binding::production::ControlledBindingContext<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(crate::RowSchema, Option<crate::RowSchema>), SQLError> {
        let analyze = |source: &uqa_sql::plan::SourcePlan| {
            uqa_sql::binding::production::analyze_source_plan_schema_with_control(
                self.returning.routines,
                source,
                params,
                binding.copy_with_control(control)?,
                None,
                control,
            )
        };
        Ok((
            analyze(&*input.target_source_with_control(control)?)?,
            input.source().map(analyze).transpose()?,
        ))
    }
}
