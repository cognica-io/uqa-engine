//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER assignment retains source-aware casts and plans only immutable base coercions.

use super::AnalyzedTypeTransform;
use crate::assignment::{conversion, AssignmentContext};
use crate::{ColumnType, SQLError, ScalarExpr};
use uqa_core::Value;

/// Convert one USING result with the original expression type, then enforce any target domain constraints. In particular, a small integer's negative value casts to OID using its declared width rather than the carrier's i64 width.
pub fn assign_type_transform_value(
    context: &dyn AssignmentContext,
    value: Value,
    target: &ColumnType,
    source: Option<&ColumnType>,
) -> Result<Value, SQLError> {
    let Some(source) = source else {
        return conversion::coerce_assignment_value(context, value, target, None);
    };
    if matches!(value, Value::Null) || source == target || target.is_character_string() {
        return conversion::coerce_assignment_value(context, value, target, Some(source));
    }
    let value = conversion::convert_declared_value_to_column_type(context, value, source, target)?;
    if contains_domain(target) {
        conversion::coerce_assignment_value(context, value, target, None)
    } else {
        Ok(value)
    }
}

/// Fold assignment of a planned constant to the base type, including its typmod. `PostgreSQL` leaves domain membership checks for row execution; an array coercion containing a domain is itself nonconstant and remains entirely deferred.
pub fn fold_type_transform_assignment(
    context: &dyn AssignmentContext,
    target: &ColumnType,
    transform: &mut AnalyzedTypeTransform,
) -> Result<(), SQLError> {
    let mut base = target;
    while let ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    if contains_domain(base) {
        return Ok(());
    }
    let value = match &transform.plan.scalar {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => value,
        _ => return Ok(()),
    };
    let value =
        assign_type_transform_value(context, value.clone(), base, transform.source_type.as_ref())?;
    transform.plan.scalar = ScalarExpr::TypedLiteral {
        composite_source: None,
        value,
        ty: base.sql_name(),
        bound_type: Some(base.clone()),
        parameter_index: None,
    };
    transform.source_type = Some(base.clone());
    Ok(())
}

fn contains_domain(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Domain { .. } => true,
        ColumnType::Array(element) => contains_domain(element),
        _ => false,
    }
}
