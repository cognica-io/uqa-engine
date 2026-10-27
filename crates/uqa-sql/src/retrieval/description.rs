//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inspect vector-call identity while leaving unsafe or unavailable arguments unevaluated.

use super::{calls, ControlledConstantEvaluator, RetrievalConstants, RetrievalExpr};
use crate::SQLParam;
use crate::{registry::FunctionKind, SQLError, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl};

#[cfg(test)]
mod tests;

pub struct VectorCallDescription<S = String, B = RetrievalExpr> {
    pub field: Option<S>,
    pub qualifier: Option<S>,
    pub calibrated: bool,
    pub bound: Option<B>,
    pub invalid_arguments: bool,
}

pub type ProducedVectorCallDescription =
    VectorCallDescription<Produced<String>, Produced<RetrievalExpr>>;

pub fn describe_vector_call(
    expression: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
    can_evaluate: &dyn Fn(&ScalarExpr) -> bool,
) -> Result<Option<VectorCallDescription>, SQLError> {
    let value = describe_vector_call_with_control(
        expression,
        constants.params,
        &|expression, params, control| {
            Ok(control.retain_external_value((constants.evaluate)(expression, params)?)?)
        },
        can_evaluate,
        &ProductionControl::uncontrolled(),
    )?;
    Ok(value.map(|value| VectorCallDescription {
        field: value
            .field
            .map(|field| field.into_uncontrolled().expect("ordinary field")),
        qualifier: value
            .qualifier
            .map(|qualifier| qualifier.into_uncontrolled().expect("ordinary qualifier")),
        calibrated: value.calibrated,
        bound: value
            .bound
            .map(|call| call.into_uncontrolled().expect("ordinary vector call")),
        invalid_arguments: value.invalid_arguments,
    }))
}

pub fn describe_vector_call_with_control(
    expression: &ScalarExpr,
    params: &[SQLParam],
    evaluate: &ControlledConstantEvaluator<'_>,
    can_evaluate: &dyn Fn(&ScalarExpr) -> bool,
    control: &ProductionControl<'_>,
) -> Result<Option<ProducedVectorCallDescription>, SQLError> {
    control.check()?;
    let ScalarExpr::Func { name, args, .. } = expression else {
        return Ok(None);
    };
    let calibrated = match crate::registry::lookup(name) {
        Some(FunctionKind::KNNMatch) => false,
        Some(FunctionKind::CalibratedVectorMatch) => true,
        _ => return Ok(None),
    };
    calls::validate_operator_function_arity(name, args.len())?;
    let field = calls::vector::field(
        &args[0],
        calibrated && can_evaluate(&args[0]),
        params,
        evaluate,
        control,
    )?;
    let qualifier = match &args[0] {
        ScalarExpr::QualifiedColumn { qualifier, .. } => Some(control.copy_text(qualifier)?),
        _ => None,
    };
    let available = field.is_some() && args.iter().skip(1).all(can_evaluate);
    let bound = if available {
        match calls::vector::bind(calibrated, args, params, evaluate, control) {
            Ok(value) => Some(value),
            Err(error) if calls::vector::resource_failure(&error) => return Err(error),
            Err(_) => None,
        }
    } else {
        None
    };
    Ok(Some(VectorCallDescription {
        field,
        qualifier,
        calibrated,
        invalid_arguments: available && bound.is_none(),
        bound,
    }))
}
