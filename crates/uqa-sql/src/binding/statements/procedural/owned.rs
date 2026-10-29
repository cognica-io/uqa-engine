//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedural cache publication moves analyzed executables and shares their exact metadata owners.

use super::{
    AnalyzedResult, PreparedAnalysisDependencies, PreparedDependencySnapshot,
    RoutineOverloadContext, RoutineTypeCatalog, SQLError, SQLParam, StatementAnalysisContext,
};
use crate::{
    binding::production::ControlledBindingContext,
    catalog::resolution::EffectiveSearchPath,
    plan::{CommandPlan, MutableExecutablePlan, OwnedExecutablePlan, PlanRef},
    prepared::{definition::PreparedDefinition, ownership::SharedPreparedInput},
    ColumnType,
};
use uqa_core::memory::{Produced, ProductionControl};

pub struct OwnedProceduralPlanAnalysis {
    pub plan: OwnedExecutablePlan,
    pub composite_inputs: crate::prepared::composites::CompositeInputs,
    pub result: AnalyzedResult,
    pub parameter_types: SharedPreparedInput<Vec<Option<ColumnType>>>,
    pub dependencies: SharedPreparedInput<PreparedAnalysisDependencies>,
    pub effective_search_path: SharedPreparedInput<Option<EffectiveSearchPath>>,
    pub dependency_snapshot: SharedPreparedInput<Option<PreparedDependencySnapshot>>,
}

impl OwnedProceduralPlanAnalysis {
    pub fn prepared_definition_owned(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<PreparedDefinition<OwnedExecutablePlan>, SQLError> {
        let result_schema = match &self.result {
            AnalyzedResult::Command => None,
            AnalyzedResult::Schema(schema) => Some(schema.clone()),
            AnalyzedResult::Rows(types) => {
                Some(crate::RowSchema::with_column_metadata_with_control(
                    None,
                    types.iter().map(|ty| ("", ty.as_ref(), None)),
                    control,
                )?)
            }
        };
        control.check()?;
        Ok(PreparedDefinition {
            composite_inputs: self.composite_inputs,
            logical_plan: self.plan,
            parameter_types: self.parameter_types,
            result_schema,
            effective_search_path: self.effective_search_path,
            dependencies: self.dependencies,
            dependency_snapshot: self.dependency_snapshot,
        })
    }
}

pub fn analyze_procedural_plan_owned(
    context: &StatementAnalysisContext<'_>,
    overloads: &RoutineOverloadContext<'_>,
    types: &dyn RoutineTypeCatalog,
    plan: MutableExecutablePlan,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<OwnedProceduralPlanAnalysis, SQLError> {
    let mut source = Some(plan);
    let mut analyzed = None;
    context
        .scopes
        .with_controlled_scope(control, &mut |binding| {
            let plan = source
                .take()
                .ok_or_else(|| SQLError::Internal("procedural analysis scope ran twice".into()))?;
            let catalog = binding.catalog.clone();
            let resolution = binding.resolution.copy_with_control(control)?;
            let metadata = binding.copy_with_control(control)?;
            let (plan, result, dependencies) =
                analyze_inputs(context, overloads, types, plan, params, binding, control)?;
            let plan = crate::routines::compilation::bind_analyzed_sql_body_types_owned(
                types, plan, control,
            )?;
            let parameter_types =
                crate::binding::parameter_input_types_with_control(params, control)?;
            let plan = crate::binding::preparation::retain_input_metadata_owned(
                context.routines,
                plan,
                params,
                metadata,
                control,
            )?;
            let plan = plan.finish(control)?;
            let composite_inputs =
                crate::prepared::composites::CompositeInputs::capture_with_control(
                    &plan,
                    context.routines,
                    control,
                )?;
            let effective_search_path =
                catalog.effective_search_path_with_control(&resolution, control)?;
            let snapshot =
                catalog.prepared_dependency_snapshot_with_control(&dependencies, control)?;
            analyzed = Some(OwnedProceduralPlanAnalysis {
                plan,
                composite_inputs,
                result,
                parameter_types: SharedPreparedInput::from_produced(parameter_types, control)?,
                dependencies: SharedPreparedInput::from_produced(dependencies, control)?,
                effective_search_path: SharedPreparedInput::from_produced(
                    effective_search_path,
                    control,
                )?,
                dependency_snapshot: SharedPreparedInput::from_produced(
                    control.finish(snapshot, control.empty_reservation())?,
                    control,
                )?,
            });
            Ok(())
        })?;
    analyzed.ok_or_else(|| SQLError::Internal("procedural analysis scope did not run".into()))
}

fn analyze_inputs(
    context: &StatementAnalysisContext<'_>,
    overloads: &RoutineOverloadContext<'_>,
    types: &dyn RoutineTypeCatalog,
    plan: MutableExecutablePlan,
    params: &[SQLParam],
    binding: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<
    (
        MutableExecutablePlan,
        AnalyzedResult,
        Produced<PreparedAnalysisDependencies>,
    ),
    SQLError,
> {
    if let PlanRef::Command(command) = plan.as_plan_ref() {
        if matches!(command, CommandPlan::Call { .. }) {
            let declared = crate::binding::parameter_input_types_with_control(params, control)?;
            let (schema, input) = crate::binding::read_procedure_inputs_owned(
                context, overloads, types, plan, &declared, binding, control,
            )?;
            return Ok((
                input.plan,
                schema.map_or(AnalyzedResult::Command, AnalyzedResult::Schema),
                input.dependencies,
            ));
        }
        if matches!(command, CommandPlan::Explain { .. }) {
            let MutableExecutablePlan::Command(command) = plan else {
                unreachable!();
            };
            let mut dependencies = None;
            let command = command.map_executable_body(control, |body| {
                let (body, _, selected) =
                    analyze_inputs(context, overloads, types, body, params, binding, control)?;
                dependencies = Some(selected);
                Ok(body)
            })?;
            return Ok((
                MutableExecutablePlan::Command(command),
                AnalyzedResult::Command,
                dependencies.expect("EXPLAIN analyzed its body"),
            ));
        }
    }
    let input = crate::binding::read_procedural_inputs_owned(
        context.routines,
        plan,
        params,
        binding,
        context.aliases,
        control,
    )?;
    Ok((input.plan, input.result, input.dependencies))
}
