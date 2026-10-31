//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite folding borrows the complete input owner until its admitted replacement is ready.

use super::{
    composite, constant_field, ColumnType, Field, Produced, ProductionControl,
    RetainedConstantValue, RetainedScalarLiteral, RoutineTypeCatalog, SQLError, ScalarExpr, Value,
};
use uqa_sql::plan::OwnedScalarExpr;

pub(in crate::optimizer::scalar) fn fold_constructor(
    expression: &ScalarExpr,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    let Some(types) = types else { return Ok(None) };
    let ScalarExpr::CompositeRow {
        items,
        binding,
        bound_type,
    } = expression
    else {
        return Ok(None);
    };
    if !items.iter().try_fold(true, |all, item| {
        Ok::<_, SQLError>(all && super::super::literal_value_with_control(item, control)?.is_some())
    })? {
        return Ok(None);
    }
    let resolved;
    let ty = match bound_type {
        Some(ty) => ty,
        None => {
            resolved = types.resolve_catalog_column_type_name_with_control(&binding.ty, control)?;
            &resolved
        }
    };
    let ColumnType::Composite(reference) = ty else {
        return Ok(None);
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
                    super::super::literal_value_with_control(&items[index], control)?
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
    literal(value, ty, control.copy_text(&binding.ty)?, None, control).map(Some)
}

pub(super) fn literal(
    value: Produced<Value>,
    ty: Produced<ColumnType>,
    declaration: Produced<String>,
    collation_oid: Option<u32>,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    if control.budget().is_none() {
        return Ok(OwnedScalarExpr::typed_literal(
            value,
            declaration,
            Some(ty),
            collation_oid,
            None,
            control,
        )?);
    }
    Ok(OwnedScalarExpr::retained_literal(
        RetainedScalarLiteral::with_control(
            RetainedConstantValue::from_produced(value, control)?,
            Some(ty),
            Some(declaration),
            collation_oid,
            control,
        )?,
        control,
    )?)
}

pub(in crate::optimizer::scalar) fn fold_field(
    expression: &ScalarExpr,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Option<Result<OwnedScalarExpr, SQLError>> {
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
        if base.declaration != Some(ty.as_str()) {
            let types = types?;
            let target = match types.resolve_catalog_column_type_name_with_control(ty, control) {
                Ok(target) => target,
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Some(Err(error))
                }
                Err(_) => return None,
            };
            if !matches!(&*target, ColumnType::Composite(reference) if reference.oid == base.type_oid)
            {
                return None;
            }
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
    // Preserve the original tuple after validating a selected field whose descriptor may change at execution.
    if base.original {
        return None;
    }
    Some((|| {
        let declaration = field.ty.catalog_name_with_control(control)?;
        let ty = field.ty.clone_with_control(control)?;
        literal(value, ty, declaration, field.collation, control)
    })())
}
