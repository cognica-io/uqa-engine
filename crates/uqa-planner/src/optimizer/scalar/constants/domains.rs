//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A constant domain coercion can be simplified while every domain in its chain is unconstrained. The invoking catalog generation owns the plan and the result keeps the declared domain identity.

use super::{
    eligibility, literal_value_with_control, ColumnType, FunctionVolatility, Produced, SQLError,
    ScalarExpr, Value,
};
use crate::OptimizerConfig;

pub(super) struct DomainConstant {
    pub value: Produced<Value>,
    pub ty: Produced<ColumnType>,
}

pub(super) fn fold(
    expression: &ScalarExpr,
    config: &OptimizerConfig<'_>,
) -> Result<Option<DomainConstant>, SQLError> {
    let ScalarExpr::Cast { expr, ty, .. } = expression else {
        return Ok(None);
    };
    let Some(catalog) = config.routine_inlining else {
        return Ok(None);
    };
    let control = &config.production;
    let Some(target) = eligibility::cast_type(ty, Some(catalog.types), control)? else {
        return Ok(None);
    };
    if !matches!(&*target, ColumnType::Domain { .. }) {
        return Ok(None);
    }
    let Some(literal) = literal_value_with_control(expr, control)? else {
        return Ok(None);
    };
    let mut base = &*target;
    while let ColumnType::Domain {
        oid, base: parent, ..
    } = base
    {
        if catalog
            .types
            .domain_has_constraints_with_control(*oid, control)?
            != Some(false)
        {
            return Ok(None);
        }
        base = parent;
    }
    if !matches!(&*literal, Value::Null) {
        let Some(source) = eligibility::scalar_type(expr, Some(catalog.routines), control)? else {
            return Ok(None);
        };
        if &*source != base
            && (!uqa_sql::type_resolution::immutable_cast_type(base)
                || uqa_sql::type_resolution::cast_volatility_with_control(&source, base, control)?
                    != FunctionVolatility::Immutable)
        {
            return Ok(None);
        }
    }
    drop(literal);
    // The catalog evaluator applies the normal source coercion and base typmod. It also handles declared composites and arrays with their original descriptors.
    let value = catalog
        .expressions
        .evaluate_constant_routine_with_control(expression, control)?;
    Ok(Some(DomainConstant { value, ty: target }))
}
