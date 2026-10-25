//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Infer ordinary `GENERIC_PLAN` placeholders without imposing prepared Bind requirements.

use super::{parameters::ParameterTypes, Preparation};
use crate::{
    binding::{BindingContext, SchemaScope},
    plan::UnifiedPlan,
    routines::RoutineResolution,
    schema::dependencies::oid_alias::OidAliasInput,
    SQLError, SQLParam,
};

pub(in crate::binding) fn read_generic_explain_inputs(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    binding: &BindingContext<'_>,
    aliases: &dyn OidAliasInput,
) -> Result<(Vec<SQLParam>, bool), SQLError> {
    let declared = crate::prepared::declared_parameter_types(routines, plan, &[])?;
    let mut scope = SchemaScope::for_analysis(binding)?;
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::with_input_constants(
            &declared,
            Some(aliases),
            routines.enum_labels(),
            routines.catalog_input_functions(),
        )
        .retain_result_metadata(),
        schema_expression: None,
    };
    analysis.plan(plan)?;
    let constants = analysis.parameters.take_input_constants();
    // References must agree with their inferred type, but ordinary EXPLAIN neither binds values nor requires every slot to acquire a type.
    let types = analysis.parameters.finish_references()?;
    let parameters = crate::prepared::analysis_parameters(&types);
    let reusable = constants.reusable_across_messages();
    constants.apply(plan, routines, &parameters)?;
    super::retain_prepared_input_metadata(routines, plan, &types, binding)?;
    Ok((parameters, reusable))
}
