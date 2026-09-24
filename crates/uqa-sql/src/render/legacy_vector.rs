//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct SQL-produced catalog vectors without discarding their array metadata.

use crate::SQLError;
use uqa_core::{LegacyVectorValue, Value};

/// Render an expression retaining the vector's kind, dimensions and bounds. SQL array functions can produce dimensionless results whose text output function rejects them.
pub fn legacy_vector_expression(vector: &LegacyVectorValue) -> Result<String, SQLError> {
    let ty = vector.kind().type_name();
    if !vector.has_vector_layout() {
        return Ok(format!("trim_array('0'::{ty}, 1)"));
    }
    let text = vector
        .elements()
        .iter()
        .map(|value| {
            let Value::Int(value) = value else {
                unreachable!("validated integer vector")
            };
            value.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ");
    let literal = format!("'{text}'::{ty}");
    match vector.as_array().lower_bounds() {
        [0] => Ok(literal),
        [1] if !vector.elements().is_empty() => Ok(format!("trim_array({literal}, 0)")),
        _ => Err(SQLError::TypeMismatch(
            "legacy vector lower bounds cannot be represented by an SQL literal".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{ArrayValue, LegacyVectorKind};

    #[test]
    fn legacy_vector_expression_round_trip_preserves_dimensions_and_bounds() {
        for kind in [LegacyVectorKind::SmallInteger, LegacyVectorKind::Oid] {
            for (elements, bounds) in [
                (vec![Value::Int(1)], vec![0]),
                (vec![Value::Int(1)], vec![1]),
                (vec![], vec![0]),
                (vec![], vec![]),
            ] {
                let vector = LegacyVectorValue::try_from_array(
                    kind,
                    ArrayValue::with_lower_bounds(elements, bounds).unwrap(),
                )
                .unwrap();
                let expression = legacy_vector_expression(&vector).unwrap();
                let crate::Statement::Select(select) =
                    crate::compile(&format!("SELECT {expression}"))
                        .unwrap()
                        .remove(0)
                else {
                    panic!("expected SELECT")
                };
                let value = crate::expr::eval(
                    &select.projections[0].expr,
                    &crate::expr::EvalContext::new(None, &[]),
                )
                .unwrap();
                let Value::LegacyVector(restored) = value else {
                    panic!("lost vector type")
                };
                assert_eq!(restored.kind(), kind);
                assert_eq!(restored.as_array(), vector.as_array());
                assert_eq!(
                    crate::catalog::expression_text::schema_expr_text(&crate::ast::Expr::Literal(
                        Value::LegacyVector(vector)
                    )),
                    expression
                );
            }
        }
    }
}
