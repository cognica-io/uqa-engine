//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-owned routine bodies admit their executable destinations directly and retain ordered input analysis through scalar extraction.

use super::{selection::InlineSelection, InlineOwnedRoutineExpression, RoutineInliningContext};
use crate::{
    ast::{CreateFunction, FunctionBinding, FunctionBody},
    binding::statements::AnalyzedResult,
    plan::{MutableExecutablePlan, OwnedScalarExpr, QueryScalarSlot, RelationalPlan, UnifiedPlan},
    routines::{CompiledFunctionBody, RoutineBody, SQLUserFunction},
    ColumnType, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl};

type OwnedInlineScalar = (OwnedScalarExpr, Produced<Vec<SQLParam>>);

pub(super) struct OwnedAnalyzedBody {
    pub plan: MutableExecutablePlan,
    pub result: AnalyzedResult,
    pub parameters: Produced<Vec<SQLParam>>,
}

impl RoutineInliningContext<'_> {
    pub fn prepare_owned(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        active: &[[u8; 16]],
        control: &ProductionControl<'_>,
    ) -> Result<Option<InlineOwnedRoutineExpression>, SQLError> {
        match self.select_inline_with_control(binding, arguments, active, control)? {
            InlineSelection::Declined => Ok(None),
            InlineSelection::Expression(expression) => Ok(Some(expression)),
            InlineSelection::Body(candidate) => {
                let Some((expression, parameters)) = self
                    .analyze_body_owned(&candidate.function, candidate.definition(), control)
                    .map_err(|error| error.with_context(candidate.error_context.as_str()))?
                else {
                    return Ok(None);
                };
                candidate.substitute(expression, &parameters, control, self)
            }
        }
    }

    fn analyze_body_owned(
        &self,
        function: &SQLUserFunction,
        definition: &CreateFunction,
        control: &ProductionControl<'_>,
    ) -> Result<Option<OwnedInlineScalar>, SQLError> {
        let Some(OwnedAnalyzedBody {
            plan,
            result,
            parameters,
        }) = self.analyze_plan_owned(function, definition, control)?
        else {
            return Ok(None);
        };
        let MutableExecutablePlan::Query(query) = plan else {
            return Ok(None);
        };
        if !query.ctes.is_empty() {
            return Ok(None);
        }
        let RelationalPlan::QueryBlock(block) = &query.root else {
            return Ok(None);
        };
        if !block.is_single_expression() {
            return Ok(None);
        }
        let layout = crate::routines::result_check::sql_function_result_layout_with_control(
            self.types,
            definition,
            Some(&result),
            control,
        )?;
        use crate::routines::result_check::SQLFunctionResultKind;
        if layout.kind == SQLFunctionResultKind::Tuple {
            return Ok(None);
        }
        let actual = result
            .column_types()
            .and_then(|types| types.first())
            .and_then(Option::as_ref);
        if layout.kind == SQLFunctionResultKind::Void && actual != Some(&ColumnType::Void) {
            return Ok(None);
        }
        let mut expression = query
            .select_scalar(QueryScalarSlot::Projection(0), control)?
            .expect("single-expression projection");
        if actual != Some(&layout.declared_type) {
            expression = OwnedScalarExpr::cast(
                expression,
                layout.declared_type.catalog_name_with_control(control)?,
                true,
                control,
            )?;
        }
        Ok(Some((expression, parameters)))
    }

    pub(super) fn analyze_plan_owned(
        &self,
        function: &SQLUserFunction,
        definition: &CreateFunction,
        control: &ProductionControl<'_>,
    ) -> Result<Option<OwnedAnalyzedBody>, SQLError> {
        let Some(plan) = self.body_plan_owned(function, control)? else {
            return Ok(None);
        };
        let parameters = crate::routines::body_validation::routine_parameter_values_with_control(
            self.types, definition, control,
        )?;
        let names = matches!(definition.body, FunctionBody::Source(_))
            .then(|| {
                crate::routines::body_parameters::sql_body_parameter_scope_with_control(
                    definition,
                    &parameters,
                    control,
                )
            })
            .transpose()?;
        let mut snapshot = self.catalog.binding_snapshot_with_control(control)?;
        let namespace = self.catalog.stored_query_namespace_with_control(control)?;
        snapshot
            .ctes
            .retain(|name, _| namespace.transition_relations.contains(name));
        snapshot.deferred_ctes.clear();
        snapshot.non_returning_ctes.clear();
        snapshot.scalar_subqueries = crate::plan::RetainedQueryArena::default();
        let prepared = {
            let _placement = if matches!(definition.body, FunctionBody::Statements(_)) {
                crate::semantics::sets::placement_scope::SetPlacementScope::stored_definition()
            } else {
                crate::semantics::sets::placement_scope::SetPlacementScope::compiled_source()
            };
            crate::binding::prepare_routine_body_inputs_owned(
                self.routines,
                plan,
                &parameters,
                snapshot.context_with_control(control)?,
                self.catalog,
                names.as_ref(),
                control,
            )?
        };
        Ok(Some(OwnedAnalyzedBody {
            plan: prepared.plan,
            result: prepared.result,
            parameters,
        }))
    }

    fn body_plan_owned(
        &self,
        function: &SQLUserFunction,
        control: &ProductionControl<'_>,
    ) -> Result<Option<MutableExecutablePlan>, SQLError> {
        control.check()?;
        match &function.body {
            RoutineBody::Bound(body) => match body.as_ref() {
                CompiledFunctionBody::SQL(plans) if plans.len() == 1 => {
                    plans[0].copy_for_rewrite(control).map(Some)
                }
                _ => Ok(None),
            },
            RoutineBody::Source => {
                let FunctionBody::Source(source) = &function.def.body else {
                    return Err(SQLError::Internal(
                        "source routine has no SQL source".into(),
                    ));
                };
                super::super::compilation::with_parser_context(self.parsers, || {
                    let statements =
                        crate::compiler::parse_statements_with_control(source, control)?;
                    let [statement] = statements.as_slice() else {
                        return Ok(None);
                    };
                    let syntax = statement.compile()?;
                    UnifiedPlan::lower_borrowed_for_rewrite(
                        &syntax,
                        &AggregateLookup(self.routines),
                        control,
                    )
                    .map(Some)
                })
            }
        }
    }
}

struct AggregateLookup<'a>(&'a dyn crate::routines::RoutineResolution);
impl crate::plan::AggregateClassifier for AggregateLookup<'_> {
    fn is_registered_aggregate(&self, name: &str) -> bool {
        self.0.has_registered_aggregate_function(name)
    }
    fn is_registered_aggregate_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.0
            .has_registered_aggregate_function_with_control(name, control)
    }
}
