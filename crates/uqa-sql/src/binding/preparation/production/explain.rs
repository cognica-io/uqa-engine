//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generic EXPLAIN infers its placeholders within the original statement owner.

use super::{
    retain_prepared_input_metadata_owned, ControlledBindingContext, MutableExecutablePlan,
    OidAliasInput, ParameterTypes, Preparation, Produced, ProductionControl, RoutineResolution,
    SQLError, SQLParam, SchemaScope,
};

pub fn read_generic_explain_inputs_owned(
    routines: &dyn RoutineResolution,
    mut plan: MutableExecutablePlan,
    binding: ControlledBindingContext<'_>,
    aliases: &dyn OidAliasInput,
    control: &ProductionControl<'_>,
) -> Result<(MutableExecutablePlan, Produced<Vec<SQLParam>>, bool), SQLError> {
    let declared = crate::prepared::declared_parameter_types_for_plan_with_control(
        routines,
        plan.as_plan_ref(),
        &[],
        control,
    )?;
    let metadata = binding.copy_with_control(control)?;
    let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::with_input_constants_with_control(
            &declared,
            Some(aliases),
            routines.enum_labels(),
            routines.catalog_input_functions(),
            control,
        )?
        .retain_result_metadata(),
        schema_expression: None,
    };
    analysis.observe_owned_plan(&plan, None)?;
    let constants = analysis.parameters.take_input_constants();
    let types = analysis.parameters.finish_references_produced()?;
    let parameters = crate::prepared::analysis_parameters_with_control(&types, control)?;
    let reusable = constants.reusable_across_messages();
    constants.apply_owned(&mut plan, routines, &parameters, control)?;
    let plan = retain_prepared_input_metadata_owned(routines, plan, &types, metadata, control)?;
    Ok((plan, parameters, reusable))
}
