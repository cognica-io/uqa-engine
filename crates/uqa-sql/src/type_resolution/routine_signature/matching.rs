//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coercion planning and ranked result construction for one routine candidate.

use super::{
    MatchedRoutineSignature, RoutineCallDescriptor, RoutineParameterDescriptor,
    RoutineSignatureMatchError,
};

/// Match one selected signature and require its defaults to have consistent types.
pub fn match_routine_signature(
    parameters: &[RoutineParameterDescriptor],
    call: RoutineCallDescriptor<'_>,
) -> Result<Option<MatchedRoutineSignature>, RoutineSignatureMatchError> {
    let matched = match_routine_candidate(parameters, call)?;
    if let Some(error) = matched
        .as_ref()
        .and_then(|matched| matched.default_error.clone())
    {
        return Err(error);
    }
    Ok(matched)
}

pub(crate) fn match_routine_candidate(
    parameters: &[RoutineParameterDescriptor],
    call: RoutineCallDescriptor<'_>,
) -> Result<Option<MatchedRoutineSignature>, RoutineSignatureMatchError> {
    use super::production::RoutineSignatureProductionError;
    match super::production::match_routine_candidate_with_control(
        parameters,
        call,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    ) {
        Ok(value) => Ok(value.map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary routine candidate")
        })),
        Err(RoutineSignatureProductionError::Signature(error)) => Err(error
            .into_uncontrolled()
            .expect("ordinary routine diagnostic")),
        Err(RoutineSignatureProductionError::Production(error)) => {
            panic!("ordinary routine candidate production failed: {error}")
        }
    }
}
