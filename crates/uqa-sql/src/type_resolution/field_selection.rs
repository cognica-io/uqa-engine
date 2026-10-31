//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The type of `(expression).field`, as `ParseComplexProjection` resolves it: a whole-row reference selects the relation's column, an anonymous row selects its `fN` field, and a composite value selects its attribute. Otherwise `unknown_attribute` reports the missing field in the form that matches the expression.

use crate::ast::ColumnType;
use crate::SQLError;

use super::FunctionTypeResolver;

/// The structural field-selection operation, independently of its rendered function name.
pub(crate) fn field_reference(
    expression: &crate::ScalarExpr,
) -> Option<(&crate::ScalarExpr, &str)> {
    let crate::ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = expression
    else {
        return None;
    };
    if binding.dispatch != Some(crate::ast::FunctionDispatch::FieldSelect) {
        return None;
    }
    let [base, crate::ScalarExpr::Literal(uqa_core::Value::Str(field))] = args.as_slice() else {
        return None;
    };
    Some((base, field))
}

/// Resolve `(relation).field` to its column without reading the other fields. A real column with the same name as the relation retains ordinary composite-value selection.
pub fn relation_column(
    expression: &crate::ScalarExpr,
    schema: &crate::RowSchema,
) -> Option<crate::ColumnIdentity> {
    relation_column_ref(expression, schema).cloned()
}

/// Borrow the selected retained schema identity without allocating a duplicate name.
pub fn relation_column_ref<'a>(
    expression: &crate::ScalarExpr,
    schema: &'a crate::RowSchema,
) -> Option<&'a crate::ColumnIdentity> {
    let (base, field) = field_reference(expression)?;
    let qualifier = whole_row_qualifier(base, schema)?;
    schema
        .qualified_position(qualifier, field)
        .and_then(|position| schema.identity(position))
}

/// Whole-row projection resolves directly against the relation namespace, before consulting a named composite descriptor.
pub(crate) fn whole_row_qualifier<'a>(
    base: &'a crate::ScalarExpr,
    schema: &dyn crate::schema::ScalarTypeSchema,
) -> Option<&'a str> {
    match base {
        crate::ScalarExpr::Column(qualifier)
            if !schema.has_unqualified_column(qualifier)
                && !schema.column_is_ambiguous(qualifier)
                && schema.has_qualifier(qualifier) =>
        {
            Some(qualifier)
        }
        crate::ScalarExpr::QualifiedStar(qualifier) if schema.has_qualifier(qualifier) => {
            Some(qualifier)
        }
        _ => None,
    }
}

/// The field of a whole-row reference that does not exist, named by the reference's alias.
#[must_use]
pub fn missing_relation_column(qualifier: &str, field: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42703".into(),
        message: format!("column {qualifier}.{field} does not exist"),
    }
}

pub(crate) fn undefined_record_field(field: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42703".into(),
        message: format!("could not identify column \"{field}\" in record data type"),
    }
}

pub(crate) fn unknown_conversion_error(target: &ColumnType) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX000".into(),
        message: format!(
            "failed to find conversion function from unknown to {}",
            super::base_type(target).display_name()
        ),
    }
}

pub(crate) fn unregistered_record() -> SQLError {
    SQLError::Routine {
        sqlstate: "42809".into(),
        message: "record type has not been registered".into(),
    }
}

pub(crate) fn is_column_reference(expression: &crate::ScalarExpr) -> bool {
    matches!(
        expression,
        crate::ScalarExpr::Column(_)
            | crate::ScalarExpr::QualifiedColumn { .. }
            | crate::ScalarExpr::InternalColumn(_)
            | crate::ScalarExpr::Position(_)
    )
}

/// The zero-based position of an anonymous row's `fN` field.
#[must_use]
pub fn anonymous_field_position(field: &str, width: usize) -> Option<usize> {
    let position = field.strip_prefix('f')?;
    if position.starts_with('0') || !position.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    position
        .parse::<usize>()
        .ok()
        .and_then(|position| position.checked_sub(1))
        .filter(|position| *position < width)
}

/// Copy only the selected attribute while its immutable catalog descriptor is borrowed.
pub(crate) fn value_field_type_with_control(
    base: Option<&ColumnType>,
    field: &str,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
    Ok(value_field_metadata_with_control(base, field, resolver, control)?.map(|(ty, _)| ty))
}

/// Retain the selected type and observe its collation from the same controlled catalog descriptor.
pub(crate) fn value_field_metadata_with_control(
    base: Option<&ColumnType>,
    field: &str,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<(uqa_core::memory::Produced<ColumnType>, u32)>, SQLError> {
    control.check()?;
    let Some(base) = base else {
        return Ok(None);
    };
    let mut composite = base;
    while let ColumnType::Domain { base, .. } = composite {
        composite = base;
    }
    match composite {
        ColumnType::Composite(reference) => {
            let Some(catalog) = resolver.and_then(FunctionTypeResolver::composite_types) else {
                return Ok(None);
            };
            let descriptor = crate::expr::composites::descriptor_with_control(
                Some(catalog),
                reference.oid,
                control,
            )?;
            let (_, attribute) = descriptor
                .attribute(field)
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!(
                        "column \"{field}\" not found in data type {}",
                        base.display_name()
                    ),
                })?;
            Ok(Some((
                attribute.ty.clone_with_control(control)?,
                attribute.collation_oid(),
            )))
        }
        ColumnType::Record => Err(undefined_record_field(field)),
        other => Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!(
                "column notation .{field} applied to type {}, which is not a composite type",
                other.display_name()
            ),
        }),
    }
}

pub(crate) fn constant_field_type_with_control(
    expression: &crate::ScalarExpr,
    field: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Option<Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError>> {
    let value = match expression {
        crate::ScalarExpr::Literal(value) => value,
        crate::ScalarExpr::RetainedLiteral(literal) if literal.declaration.is_none() => {
            literal.value.resident_value()?
        }
        _ => return None,
    };
    match value {
        uqa_core::Value::Row(values) => Some((|| {
            let position = anonymous_field_position(field, values.len())
                .ok_or_else(|| undefined_record_field(field))?;
            super::common::value_type_with_control(&values[position], control)
        })()),
        uqa_core::Value::Record(fields) => Some((|| {
            let (_, value) = fields
                .iter()
                .find(|(name, _)| name.as_str() == field)
                .ok_or_else(|| undefined_record_field(field))?;
            super::common::value_type_with_control(value, control)
        })()),
        _ => None,
    }
}

pub(crate) struct SelectedValueField {
    pub number: i16,
    pub ty: ColumnType,
    pub collation: u32,
}

/// Keep the physical attribute number beside its type and collation from the same descriptor.
pub(crate) fn value_field_selection(
    base: Option<&ColumnType>,
    field: &str,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<SelectedValueField>, SQLError> {
    let Some(base) = base else {
        return Ok(None);
    };
    let mut composite = base;
    while let ColumnType::Domain { base, .. } = composite {
        composite = base;
    }
    match composite {
        ColumnType::Composite(reference) => {
            let Some(catalog) = resolver.and_then(FunctionTypeResolver::composite_types) else {
                return Ok(None);
            };
            let descriptor = crate::expr::composites::descriptor(Some(catalog), reference.oid)?;
            descriptor
                .attribute(field)
                .map(|(_, attribute)| {
                    Some(SelectedValueField {
                        number: attribute.number,
                        ty: attribute.ty.clone(),
                        collation: attribute.collation_oid(),
                    })
                })
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!(
                        "column \"{field}\" not found in data type {}",
                        base.display_name()
                    ),
                })
        }
        ColumnType::Record => Err(undefined_record_field(field)),
        other => Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!(
                "column notation .{field} applied to type {}, which is not a composite type",
                other.display_name()
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        plan::{RelationalPlan, UnifiedPlan},
        ColumnIdentity, RowSchema,
    };

    #[test]
    fn retained_anonymous_row_fields_preserve_literal_types_and_missing_field_errors() {
        use crate::ast::{FunctionBinding, FunctionDispatch, FunctionOrderSyntax};
        use crate::ir::{RetainedConstantValue, RetainedScalarLiteral};
        use crate::ScalarExpr;
        use uqa_core::{memory::ProductionControl, Value};

        let control = ProductionControl::uncontrolled();
        let value = Value::Row(vec![Value::Int(1), Value::Str("x".repeat(1 << 20))].into());
        let retained = ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
            RetainedConstantValue::from_produced(control.copy_value(&value).unwrap(), &control)
                .unwrap(),
            Some(ColumnType::Record),
            None,
            None,
        ));
        let ordinary = ScalarExpr::Literal(value);
        let binding = FunctionBinding::dispatched(FunctionDispatch::FieldSelect);
        for field in ["f1", "f2", "f0", "f01", "f3", "missing"] {
            let call = |base| ScalarExpr::Func {
                name: binding.name.clone(),
                binding: Some(binding.clone()),
                args: vec![base, ScalarExpr::Literal(Value::Str(field.into()))],
                distinct: false,
                order_syntax: FunctionOrderSyntax::Ordinary,
                order_by: Vec::new(),
                filter: None,
            };
            let expected = crate::scalar_type(&call(ordinary.clone()), &RowSchema::default(), &[]);
            let expression = call(retained.clone());
            let measured = allocation_counter::measure(|| {
                let actual = crate::scalar_type(&expression, &RowSchema::default(), &[]);
                assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
            });
            assert!(measured.bytes_total < 4096, "{measured:?}");
        }
    }

    #[test]
    fn relation_field_selection_respects_column_shadowing_and_structural_dispatch() {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(crate::compile("SELECT (s).k").unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("block")
        };
        let expression = &block.projections[0].expr;
        let schema =
            RowSchema::with_qualified_types("s", vec!["k".into()], vec![Some(ColumnType::Integer)]);
        assert_eq!(
            relation_column(expression, &schema),
            Some(ColumnIdentity::qualified("s", "k"))
        );
        let shadowed = RowSchema::with_qualified_types(
            "s",
            vec!["k".into(), "s".into()],
            vec![Some(ColumnType::Integer), Some(ColumnType::Record)],
        );
        assert!(relation_column(expression, &shadowed).is_none());
        let mut ordinary_call = expression.clone();
        if let crate::ScalarExpr::Func { binding, .. } = &mut ordinary_call {
            *binding = None;
        }
        assert!(relation_column(&ordinary_call, &schema).is_none());
    }
}
