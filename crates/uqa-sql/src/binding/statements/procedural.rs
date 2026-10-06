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
    pub result: AnalyzedResult,
    pub dependencies: PreparedAnalysisDependencies,
    pub effective_search_path: Option<EffectiveSearchPath>,
    pub dependency_snapshot: Option<PreparedDependencySnapshot>,
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
        let effective_search_path = binding.catalog.effective_search_path(&binding.resolution)?;
        let dependency_snapshot = binding
            .catalog
            .prepared_dependency_snapshot(&dependencies)?;
        analyzed = Some(ProceduralPlanAnalysis {
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
