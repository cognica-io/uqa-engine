//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped composite observations produce independent replacements while the original analysis addresses remain stable.

use super::SchemaScope;
use crate::{
    ast::{
        CompositeFieldBinding, FunctionBinding, FunctionDispatch, FunctionValueBinding,
        UserTypeIdentity,
    },
    plan::OwnedScalarExpr,
    production::Buffer,
    routines::RoutineResolution,
    type_resolution::{FunctionBindingParts, FunctionTypeResolver},
    ColumnType, RowSchema, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::{
    memory::{ProductionControl, ProductionVec},
    Value,
};

pub(in crate::binding) fn selected(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Cast { .. } | ScalarExpr::TypedLiteral { .. } | ScalarExpr::CompositeRow { .. }
    ) || matches!(expression, ScalarExpr::Func { binding:Some(binding), .. } if matches!(binding.dispatch, Some(FunctionDispatch::FieldSelect | FunctionDispatch::IsDistinct | FunctionDispatch::ValueNullTest { .. })))
}

impl SchemaScope {
    pub(in crate::binding) fn retain_composite_inputs_in_scope_owned(
        &mut self,
        routines: &dyn RoutineResolution,
        expression: OwnedScalarExpr,
        schema: &RowSchema,
        subqueries: &[crate::plan::QueryPlan],
        params: &[SQLParam],
    ) -> Result<OwnedScalarExpr, SQLError> {
        let owner = self.control().capture();
        let control = owner.control();
        let expression = expression.transform_nodes(&control, &mut |_, expression| {
            canonical_type(expression, routines, &control)
        })?;
        let resolver = self.query_function_type_resolver_for_subqueries(
            routines,
            std::slice::from_ref(&*expression),
            schema,
            subqueries,
            params,
        )?;
        expression.transform_nodes(&control, &mut |_, expression| {
            let expression = crate::type_resolution::composite_rows::bind_stored_row_owned(
                expression, schema, params, &resolver, &control,
            )?;
            let expression = retain_field(expression, schema, params, &resolver, &control)?;
            retain_comparison(expression, schema, params, &resolver, &control)
        })
    }
}

fn canonical_type(
    expression: OwnedScalarExpr,
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let name = match &*expression {
        ScalarExpr::Cast { ty, .. }
        | ScalarExpr::TypedLiteral { ty, .. }
        | ScalarExpr::CompositeRow {
            binding: crate::ast::CompositeRowBinding { ty, .. },
            ..
        } => ty,
        _ => return Ok(expression),
    };
    if UserTypeIdentity::parse(name).is_some() {
        return Ok(expression);
    }
    let Some(ty) = resolver.resolve_type_name_with_control(name, control)? else {
        return Ok(expression);
    };
    let selected = ty.catalog_name_with_control(control)?;
    if selected.as_str() == name {
        return Ok(expression);
    }
    Ok(expression.replace_type_name(selected, control)?)
}

fn retain_field(
    expression: OwnedScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = &*expression
    else {
        return Ok(expression);
    };
    if binding.dispatch != Some(FunctionDispatch::FieldSelect)
        || binding.composite_field().is_some()
    {
        return Ok(expression);
    }
    let [base, ScalarExpr::Literal(Value::Str(name))] = args.as_slice() else {
        return Ok(expression);
    };
    let Some(ty) = crate::scalar_type_with_resolver_and_control(
        base,
        schema,
        params,
        Some(resolver),
        control,
    )?
    else {
        return Ok(expression);
    };
    let ColumnType::Composite(reference) = crate::type_resolution::base_type(&ty) else {
        return Ok(expression);
    };
    let descriptor = crate::expr::composites::descriptor_with_control(
        resolver.composite_types(),
        reference.oid,
        control,
    )?;
    let Some((_, attribute)) = descriptor.attribute(name) else {
        return Ok(expression);
    };
    let mut selected = binding_parts(binding, control)?;
    let (result_type, memory) = attribute.ty.clone_with_control(control)?.into_parts();
    selected.value_binding = Some(control.finish(
        FunctionValueBinding::CompositeField(CompositeFieldBinding {
            collation_oid: Some(attribute.collation_oid()),
            type_oid: reference.oid,
            number: attribute.number,
            result_type,
            dropped: false,
            changed_type: None,
        }),
        memory,
    )?);
    Ok(expression.replace_call_binding(Some(selected.finish(control)?), control)?)
}

fn retain_comparison(
    expression: OwnedScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = &*expression
    else {
        return Ok(expression);
    };
    if matches!(
        binding.dispatch,
        Some(FunctionDispatch::ValueNullTest { .. })
    ) {
        return retain_null_test(expression, schema, params, resolver, control);
    }
    if binding.dispatch != Some(FunctionDispatch::IsDistinct) {
        return Ok(expression);
    }
    let [left, right] = args.as_slice() else {
        return Ok(expression);
    };
    let (Some(left), Some(right)) = (
        fields(left, resolver, control)?,
        fields(right, resolver, control)?,
    ) else {
        return Ok(expression);
    };
    crate::type_resolution::row_comparison::validate_width(left.len(), right.len(), true)?;
    for (left, right) in left.iter().zip(right) {
        let left_type = crate::scalar_type_with_resolver_and_control(
            left,
            schema,
            params,
            Some(resolver),
            control,
        )?;
        let right_type = crate::scalar_type_with_resolver_and_control(
            right,
            schema,
            params,
            Some(resolver),
            control,
        )?;
        crate::type_resolution::binary_operator_types_with_control(
            crate::ast::BinaryOp::Equal,
            left_type.as_deref(),
            right_type.as_deref(),
            control,
        )?;
    }
    let (parent, arguments) = expression.take_call_arguments(control)?;
    drop(parent);
    let mut arguments = arguments.into_children();
    let left = take_fields(arguments.next().expect("left row"));
    let right = take_fields(arguments.next().expect("right row"));
    drop(arguments);
    let mut comparisons = Buffer::new(control);
    for (left, right) in left.into_children().zip(right.into_children()) {
        comparisons.push(
            call(
                FunctionDispatch::ValueIsDistinct,
                [left, right].into_iter(),
                control,
            )?,
            control,
        )?;
    }
    let expression = match comparisons.len() {
        0 => ScalarExpr::Literal(Value::Bool(false)).copy_for_rewrite(control)?,
        1 => comparisons.pop().expect("single row comparison"),
        _ => call(
            FunctionDispatch::RowDistinct,
            comparisons.into_iter(),
            control,
        )?,
    };
    crate::type_resolution::bind_type_introspection_owned(
        expression,
        schema,
        params,
        Some(resolver),
        control,
    )
}

fn fields<'a>(
    expression: &'a ScalarExpr,
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Option<&'a [ScalarExpr]>, SQLError> {
    Ok(match expression {
        ScalarExpr::Row(items) | ScalarExpr::CompositeRow { items, .. } => Some(items),
        ScalarExpr::Cast {
            expr,
            ty,
            implicit: false,
        } if resolver
            .resolve_type_name_with_control(ty, control)?
            .as_deref()
            == Some(&ColumnType::Record) =>
        {
            match expr.as_ref() {
                ScalarExpr::Row(items) | ScalarExpr::CompositeRow { items, .. } => Some(items),
                _ => None,
            }
        }
        _ => None,
    })
}

fn take_fields(mut expression: OwnedScalarExpr) -> OwnedScalarExpr {
    while matches!(&*expression, ScalarExpr::Cast { .. }) {
        expression = expression.select_child(0);
    }
    expression
}

fn call(
    dispatch: FunctionDispatch,
    arguments: impl ExactSizeIterator<Item = OwnedScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let binding = FunctionBindingParts::dispatched(dispatch, control)?.finish(control)?;
    Ok(OwnedScalarExpr::call(
        control.copy_text(dispatch.label())?,
        Some(binding),
        arguments,
        control,
    )?)
}

fn binding_parts(
    binding: &FunctionBinding,
    control: &ProductionControl<'_>,
) -> Result<FunctionBindingParts, SQLError> {
    let mut types = ProductionVec::new(*control);
    types.reserve(binding.argument_types.len())?;
    for ty in &binding.argument_types {
        types.push_produced(control.copy_text(ty)?)?;
    }
    Ok(FunctionBindingParts {
        object_id: binding.object_id,
        name: control.copy_text(&binding.name)?,
        argument_types: types.finish()?,
        builtin: binding.builtin,
        dispatch: binding.dispatch,
        invocation: binding
            .invocation
            .as_deref()
            .map(|value| crate::plan::copy::copy_syntax_with_control(value, control))
            .transpose()?,
        value_binding: binding
            .value_binding
            .as_deref()
            .map(|value| crate::plan::copy::copy_syntax_with_control(value, control))
            .transpose()?,
        resolution_error: binding
            .resolution_error
            .as_deref()
            .map(|value| crate::plan::copy::copy_syntax_with_control(value, control))
            .transpose()?,
        input_collation: binding.input_collation,
    })
}

fn retain_null_test(
    expression: OwnedScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = &*expression
    else {
        unreachable!("bound null-test call");
    };
    if let [argument] = args.as_slice() {
        if let Some(ty) = crate::scalar_type_with_resolver_and_control(
            argument,
            schema,
            params,
            Some(resolver),
            control,
        )? {
            let mut selected = binding_parts(binding, control)?;
            let mut types = ProductionVec::new(*control);
            types.push_produced(match ty.user_type_identity_with_control(control)? {
                Some(name) => name,
                None => ty.catalog_name_with_control(control)?,
            })?;
            selected.argument_types = types.finish()?;
            return Ok(expression.replace_call_binding(Some(selected.finish(control)?), control)?);
        }
    }
    Ok(expression)
}
