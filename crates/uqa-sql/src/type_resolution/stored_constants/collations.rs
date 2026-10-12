//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Store analyzed coercions beneath the selected outer collation, as `PostgreSQL`'s `coerce_to_target_type` does.

use super::{FunctionTypeResolver, SQLError, ScalarExpr, Value};
use crate::catalog::collations::expression::{discard_scalar_clause, scalar_clause};

pub(super) fn normalize_cast(
    expression: &mut ScalarExpr,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    let ScalarExpr::Cast { expr, ty, .. } = expression else {
        return Ok(false);
    };
    if scalar_clause(expr)?.is_none() {
        return Ok(false);
    }
    let target = crate::type_resolution::inference::resolved_type_name(
        ty,
        Some(resolver),
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?;
    let Some(target) = target.as_deref() else {
        return Ok(false);
    };
    if crate::schema::composites::type_is_collatable(target) {
        let mut clause = std::mem::replace(expr.as_mut(), ScalarExpr::Literal(Value::Null));
        let ScalarExpr::Func { args, .. } = &mut clause else {
            unreachable!("validated collation clause");
        };
        while discard_scalar_clause(&mut args[0])? {}
        std::mem::swap(expr.as_mut(), &mut args[0]);
        std::mem::swap(expression, &mut args[0]);
        *expression = clause;
    } else {
        while discard_scalar_clause(expr)? {}
    }
    Ok(true)
}
