//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Input conversion shared by stored column and routine defaults.

use crate::{
    ast::{ColumnType, Expr},
    SQLError,
};
use uqa_core::Value;

/// Read an `unknown` literal with the input function of `target`, which reports what the type's input rejects, and store the typed constant. `coerce_type` passes interval modifiers to input; other length and precision modifiers apply when a row is assigned. The constant keeps a written cast's modifier when `keep_modifier` says so.
pub fn read_unknown_stored_literal(
    catalog: Option<&dyn crate::expr::enums::EnumLabelCatalog>,
    inputs: Option<&dyn crate::expr::CatalogInputFunctions>,
    expression: &mut Expr,
    target: &ColumnType,
    keep_modifier: bool,
) -> Result<(), SQLError> {
    if crate::catalog::stored_ast::fold_assigned_stored_literal(expression, target, catalog)? {
        return Ok(());
    }
    let Expr::Literal(value) = &*expression else {
        return Ok(());
    };
    let mut base = target;
    while let ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    if !crate::expr::requires_domain_array_input(base)
        && crate::type_resolution::catalog_input_type(base)
    {
        // The input function of an OID alias type resolves the name in the catalog, which the binding of the stored expression does; the literal takes the cast that binding resolves.
        let literal = std::mem::replace(expression, Expr::Literal(Value::Null));
        *expression = Expr::Cast {
            implicit: true,
            expr: Box::new(literal),
            ty: base.catalog_name(),
        };
        return Ok(());
    }
    let input_type = crate::type_resolution::literal_input_type_with_control(
        base,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary stored input has no reservation");
    let value = match value {
        Value::Str(text) if crate::expr::requires_domain_array_input(&input_type) => inputs
            .ok_or_else(|| {
                SQLError::Internal("stored domain array input requires catalog functions".into())
            })?
            .read_unknown_input(text, &input_type)?,
        value => {
            crate::assignment::conversion::convert_value_to_column_type(value.clone(), &input_type)?
        }
    };
    let ty = if keep_modifier { base } else { &input_type };
    *expression = Expr::TypedLiteral {
        composite_source: None,
        value,
        ty: ty.catalog_name(),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_input_applies_the_declared_fields_before_storage() {
        let target = ColumnType::from_sql_name("interval year").unwrap();
        let mut expression = Expr::Literal(Value::Str("1-2".into()));
        read_unknown_stored_literal(None, None, &mut expression, &target, false).unwrap();
        assert!(matches!(
            expression,
            Expr::TypedLiteral {
                value: Value::Temporal(uqa_core::TemporalValue::Interval {
                    months: 12,
                    days: 0,
                    micros: 0
                }),
                ..
            }
        ));
    }

    #[test]
    fn unknown_assignment_reads_base_input_and_keeps_null_default() {
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "positive".into(),
            oid: 16384,
            array_oid: None,
            base: Box::new(ColumnType::Integer),
        };
        let mut expression = Expr::Literal(Value::Str("0".into()));
        read_unknown_stored_literal(None, None, &mut expression, &domain, false).unwrap();
        assert!(
            matches!(expression, Expr::TypedLiteral { value: Value::Int(0), ty, .. } if ty == "integer")
        );
        let mut null = Expr::Literal(Value::Null);
        read_unknown_stored_literal(None, None, &mut null, &ColumnType::Integer, false).unwrap();
        assert!(
            matches!(null, Expr::TypedLiteral { value: Value::Null, ty, .. } if ty == "integer")
        );
        let mut invalid = Expr::Literal(Value::Str("bad".into()));
        assert_eq!(
            read_unknown_stored_literal(None, None, &mut invalid, &domain, false)
                .unwrap_err()
                .sqlstate(),
            Some("22P02")
        );
    }
}
