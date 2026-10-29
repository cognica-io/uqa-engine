//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fold named ROW constructors only with the current catalog and admitted constant arguments.

use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::ir::{RetainedConstantValue, RetainedScalarLiteral};
use uqa_sql::routines::declaration::RoutineTypeCatalog;
use uqa_sql::{ColumnType, SQLError, ScalarExpr};

struct Field<'a> {
    type_oid: u32,
    ty: &'a ColumnType,
    collation: Option<u32>,
    binding: Option<&'a uqa_sql::ast::CompositeFieldBinding>,
}

fn constant_field(
    value: &Value,
    name: &str,
    field: &Field<'_>,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>, SQLError>> {
    if matches!(value, Value::Null) || field.binding.is_some_and(|field| field.dropped) {
        return Some(
            control
                .finish(Value::Null, control.empty_reservation())
                .map_err(Into::into),
        );
    }
    if let Some(binding) = field.binding {
        if let Err(error) = uqa_sql::expr::composites::validate_field_result(binding) {
            return Some(Err(error));
        }
    }
    if let Value::Datum(datum) = value {
        let selected = match uqa_sql::expr::datums::read_composite_field_with_control(
            datum,
            name,
            types?.composite_types(),
            control,
        ) {
            Ok(value) => value,
            Err(error) => return Some(Err(error)),
        };
        return Some(uqa_sql::expr::datums::copy_constant_field_with_control(
            &selected, field.ty, control,
        ));
    }
    match value {
        Value::Record(fields) => {
            if fields.type_oid().is_some_and(|oid| oid != field.type_oid) {
                return None;
            }
            Some(uqa_sql::expr::datums::copy_constant_field_with_control(
                fields.iter().find(|(key, _)| key.as_str() == name)?.1,
                field.ty,
                control,
            ))
        }
        _ => None,
    }
}

pub(in crate::optimizer::scalar) fn fold_composite_constructor(
    expression: ScalarExpr,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<ScalarExpr, SQLError> {
    let Some(types) = types else {
        return Ok(expression);
    };
    let ScalarExpr::CompositeRow {
        items,
        binding,
        bound_type,
    } = &expression
    else {
        return Ok(expression);
    };
    if !items.iter().try_fold(true, |all, item| {
        Ok::<_, SQLError>(all && super::literal_value_with_control(item, control)?.is_some())
    })? {
        return Ok(expression);
    }
    let resolved;
    let ty = match bound_type {
        Some(ty) => ty,
        None => {
            resolved = types.resolve_catalog_column_type_name(&binding.ty)?;
            &resolved
        }
    };
    let ColumnType::Composite(reference) = ty else {
        return Ok(expression);
    };
    let ty = ty.clone_with_control(control)?;
    let descriptor = uqa_sql::expr::composites::descriptor(types.composite_types(), reference.oid)?;
    let value = uqa_sql::expr::composites::constructor::construct_with_control(
        binding,
        items.len(),
        &descriptor,
        control,
        |index| {
            control
                .copy_value(
                    super::literal_value_with_control(&items[index], control)?
                        .as_deref()
                        .expect("constant argument"),
                )
                .map_err(Into::into)
        },
    )?;
    let value = uqa_sql::expr::composites::retain_value_with_control(
        value,
        types.composite_types(),
        control,
    )?;
    literal(value, ty, control.copy_text(&binding.ty)?, None, control)
}

fn literal(
    value: Produced<Value>,
    ty: Produced<ColumnType>,
    declaration: Produced<String>,
    collation_oid: Option<u32>,
    control: &ProductionControl<'_>,
) -> Result<ScalarExpr, SQLError> {
    if control.budget().is_none() {
        return Ok(ScalarExpr::TypedLiteral {
            value: value.into_uncontrolled().expect("ordinary constant value"),
            ty: declaration
                .into_uncontrolled()
                .expect("ordinary type declaration"),
            bound_type: Some(ty.into_uncontrolled().expect("ordinary result type")),
            parameter_index: None,
            collation_oid,
            composite_source: None,
        });
    }
    Ok(ScalarExpr::RetainedLiteral(
        RetainedScalarLiteral::with_control(
            RetainedConstantValue::from_produced(value, control)?,
            Some(ty),
            Some(declaration),
            collation_oid,
            control,
        )?,
    ))
}

struct Composite<'a> {
    value: &'a Value,
    type_oid: u32,
    declaration: Option<&'a str>,
    original: bool,
}

fn composite(expression: &ScalarExpr) -> Option<Composite<'_>> {
    let (value, ty, declaration, original) = match expression {
        ScalarExpr::TypedLiteral {
            value,
            bound_type: Some(ty),
            ty: declaration,
            composite_source,
            ..
        } => (
            value,
            ty,
            Some(declaration.as_str()),
            composite_source.is_some(),
        ),
        ScalarExpr::RetainedLiteral(literal) => (
            literal.value.resident_value()?,
            literal.ty.as_ref()?,
            literal.declaration.as_deref(),
            false,
        ),
        _ => return None,
    };
    let ColumnType::Composite(reference) = ty else {
        return None;
    };
    Some(Composite {
        value,
        type_oid: reference.oid,
        declaration,
        original,
    })
}

pub(super) fn fold_field(
    expression: &ScalarExpr,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Option<Result<ScalarExpr, SQLError>> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = expression
    else {
        return None;
    };
    if binding.dispatch != Some(uqa_sql::ast::FunctionDispatch::FieldSelect) {
        return None;
    }
    let [base, ScalarExpr::Literal(Value::Str(name))] = args.as_slice() else {
        return None;
    };
    let base = if let ScalarExpr::Cast { expr, ty, .. } = base {
        let base = composite(expr)?;
        if base.declaration != Some(ty.as_str())
            && !types.is_some_and(|types| types.resolve_catalog_column_type_name(ty).is_ok_and(|target|
                matches!(target, ColumnType::Composite(reference) if reference.oid == base.type_oid))) {
            return None;
        }
        base
    } else {
        composite(base)?
    };
    let descriptor;
    let field = if let Some(field) = binding.composite_field() {
        Field {
            type_oid: field.type_oid,
            ty: &field.result_type,
            collation: field.collation_oid,
            binding: Some(field),
        }
    } else {
        descriptor =
            match uqa_sql::expr::composites::descriptor(types?.composite_types(), base.type_oid) {
                Ok(descriptor) => descriptor,
                Err(error) => return Some(Err(error)),
            };
        let (_, attribute) = descriptor.attribute(name)?;
        Field {
            type_oid: base.type_oid,
            ty: &attribute.ty,
            collation: Some(attribute.collation_oid()),
            binding: None,
        }
    };
    if base.type_oid != field.type_oid {
        return None;
    }
    let value = match constant_field(base.value, name, &field, types, control)? {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    if base.original {
        // Validate the selected constant now, then retain its original tuple so later descriptors can still recompute its byte position.
        return None;
    }
    Some((|| {
        let declaration = field.ty.catalog_name_with_control(control)?;
        let ty = field.ty.clone_with_control(control)?;
        literal(value, ty, declaration, field.collation, control)
    })())
}
