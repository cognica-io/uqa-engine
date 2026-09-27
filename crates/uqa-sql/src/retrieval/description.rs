//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inspect vector-call identity while leaving unsafe or unavailable arguments unevaluated.

use super::{calls, column_name, RetrievalConstants, RetrievalExpr};
use crate::{registry::FunctionKind, SQLError, ScalarExpr};

#[cfg(test)]
mod tests;

pub struct VectorCallDescription {
    pub field: Option<String>,
    pub qualifier: Option<String>,
    pub calibrated: bool,
    pub bound: Option<RetrievalExpr>,
    pub invalid_arguments: bool,
}

pub fn describe_vector_call(
    expression: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
    can_evaluate: &dyn Fn(&ScalarExpr) -> bool,
) -> Result<Option<VectorCallDescription>, SQLError> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return Ok(None);
    };
    let calibrated = match crate::registry::lookup(name) {
        Some(FunctionKind::KNNMatch) => false,
        Some(FunctionKind::CalibratedVectorMatch) => true,
        _ => return Ok(None),
    };
    calls::validate_operator_function_arity(name, args.len())?;
    let field = column_name(&args[0]).or_else(|| {
        (calibrated && can_evaluate(&args[0]))
            .then(|| calls::field_name_arg(&args[0], constants))
            .flatten()
    });
    let qualifier = match &args[0] {
        ScalarExpr::QualifiedColumn { qualifier, .. } => Some(qualifier.clone()),
        _ => None,
    };
    let available = field.is_some() && args.iter().skip(1).all(can_evaluate);
    let bound = if available {
        if calibrated {
            calls::try_lower_calibrated_vector_match(args, constants).ok()
        } else {
            calls::try_lower_knn_match(args, constants).ok()
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
