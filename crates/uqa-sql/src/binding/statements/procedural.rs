//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained procedural input analysis and its selected catalog dependencies.

use super::{AnalyzedResult, BindingContext, StatementAnalysisContext};
use crate::{
    catalog::resolution::EffectiveSearchPath,
    plan::{CommandPlan, UnifiedPlan},
    prepared::dependencies::{PreparedAnalysisDependencies, PreparedDependencySnapshot},
    routines::{declaration::RoutineTypeCatalog, resolution::RoutineOverloadContext},
    SQLError, SQLParam,
};

#[derive(Debug)]
pub struct ProceduralPlanAnalysis {
    pub composite_inputs: crate::prepared::composites::CompositeInputs,
    pub result: AnalyzedResult,
    pub dependencies: PreparedAnalysisDependencies,
    pub effective_search_path: Option<EffectiveSearchPath>,
    pub dependency_snapshot: Option<PreparedDependencySnapshot>,
}

impl ProceduralPlanAnalysis {
    /// Publish executable variants from the already successful procedural analysis.
    pub fn prepared_definition(
        &self,
        plan: &UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<crate::prepared::definition::PreparedDefinition, SQLError> {
        let parameter_types = (1..=params.len())
            .map(|index| {
                crate::type_resolution::scalar_type(
                    &crate::ScalarExpr::Param(index),
                    &crate::RowSchema::new(Vec::new()),
                    params,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result_schema = match &self.result {
            AnalyzedResult::Command => None,
            AnalyzedResult::Schema(schema) => Some(schema.clone()),
            AnalyzedResult::Rows(types) => Some(crate::RowSchema::with_types(
                vec![String::new(); types.len()],
                types.clone(),
            )),
        };
        Ok(crate::prepared::definition::PreparedDefinition {
            composite_inputs: self.composite_inputs.clone(),
            logical_plan: plan.clone(),
            parameter_types,
            result_schema,
            effective_search_path: self.effective_search_path.clone(),
            dependencies: self.dependencies.clone(),
            dependency_snapshot: self.dependency_snapshot.clone(),
        })
    }
}

/// Analyze a reached SQL occurrence once in its retained catalog scope. CALL
/// resolves and converts its selected inputs before successful publication too.
pub fn analyze_procedural_plan(
    context: &StatementAnalysisContext<'_>,
    overloads: &RoutineOverloadContext<'_>,
    types: &dyn RoutineTypeCatalog,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
) -> Result<ProceduralPlanAnalysis, SQLError> {
    let mut analyzed = None;
    context.scopes.with_scope(&mut |scope| {
        let binding = scope.binding_context()?;
        let (result, dependencies) =
            analyze_inputs(context, overloads, types, plan, params, &binding)?;
        crate::routines::compilation::bind_analyzed_sql_body_types(types, plan)?;
        super::super::composite_inputs::retain_composite_inputs(
            context.routines,
            plan,
            params,
            &binding,
        )?;
        let composite_inputs =
            crate::prepared::composites::CompositeInputs::capture(plan, context.routines)?;
        let effective_search_path = binding.catalog.effective_search_path(&binding.resolution)?;
        let dependency_snapshot = binding
            .catalog
            .prepared_dependency_snapshot(&dependencies)?;
        analyzed = Some(ProceduralPlanAnalysis {
            composite_inputs,
            result,
            dependencies,
            effective_search_path,
            dependency_snapshot,
        });
        Ok(())
    })?;
    analyzed.ok_or_else(|| SQLError::Internal("procedural analysis scope did not run".into()))
}

fn analyze_inputs(
    context: &StatementAnalysisContext<'_>,
    overloads: &RoutineOverloadContext<'_>,
    types: &dyn RoutineTypeCatalog,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
    binding: &BindingContext<'_>,
) -> Result<(AnalyzedResult, PreparedAnalysisDependencies), SQLError> {
    if let UnifiedPlan::Command(command) = plan {
        match command.as_mut() {
            CommandPlan::Call { .. } => {
                return crate::binding::preparation::prepare_procedure_call(
                    context, overloads, types, plan, params, binding,
                );
            }
            CommandPlan::Explain { body, .. } => {
                let (_, dependencies) =
                    analyze_inputs(context, overloads, types, body, params, binding)?;
                return Ok((AnalyzedResult::Command, dependencies));
            }
            _ => {}
        }
    }
    let dependencies = crate::binding::preparation::read_procedural_inputs(
        context.routines,
        plan,
        params,
        binding,
        context.aliases,
    )?;
    let result = super::analyze_bound_result(context.routines, plan, params, binding)?;
    Ok((result, dependencies))
}
