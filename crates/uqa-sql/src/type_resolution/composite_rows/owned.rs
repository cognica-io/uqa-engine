//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored row conversion admits selected metadata before transferring each field's scalar constructor.

use crate::{
    ast::{CompositeRowBinding, CompositeRowLayout},
    plan::OwnedScalarExpr,
    production::Buffer,
    schema::ScalarTypeSchema,
    ColumnType, FunctionTypeResolver, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(crate) fn bind_stored_row_owned(
    expression: OwnedScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    if matches!(&*expression, ScalarExpr::CompositeRow { .. }) {
        return retain_argument_types(expression, resolver, control);
    }
    let ScalarExpr::Cast { expr, ty, implicit } = &*expression else {
        return Ok(expression);
    };
    if anonymous_depth(expr, resolver, control)?.is_none() {
        return Ok(expression);
    }
    let Some(target) = resolver.resolve_type_name_with_control(ty, control)? else {
        return Ok(expression);
    };
    let ColumnType::Composite(reference) = crate::type_resolution::base_type(&target) else {
        return Ok(expression);
    };
    let descriptor = crate::expr::composites::descriptor_with_control(
        resolver.composite_types(),
        reference.oid,
        control,
    )?;
    let domain = matches!(*target, ColumnType::Domain { .. });
    let implicit = domain || *implicit;
    let name = if domain {
        crate::type_resolution::base_type(&target)
            .user_type_identity_with_control(control)?
            .expect("composite identity")
    } else {
        control.copy_text(ty)?
    };
    let convert = |mut row: OwnedScalarExpr| -> Result<OwnedScalarExpr, SQLError> {
        while matches!(&*row, ScalarExpr::Cast { .. }) {
            row = row.select_child(0);
        }
        let width = row.child_count();
        let mut children = row.into_children();
        let mut converted = Buffer::new(control);
        for (index, attribute) in descriptor.attributes.iter().enumerate() {
            let Some(item) = children.next() else {
                return Err(row_error(
                    &target,
                    "Input has too few columns.",
                    resolver,
                    control,
                )?);
            };
            let source = crate::type_resolution::scalar_type_with_resolver_and_control(
                &item,
                schema,
                params,
                Some(resolver),
                control,
            )?;
            validate_field_cast_with_control(
                &target,
                source.as_deref(),
                &attribute.ty,
                index + 1,
                resolver,
                control,
            )?;
            let item = coerce_field(item, source.as_deref(), &attribute.ty, control)?;
            let item = super::super::introspection::bind_stored_inputs_owned(
                item, schema, params, resolver, control,
            )?;
            let item = bind_stored_row_owned(item, schema, params, resolver, control)?;
            converted.push(item, control)?;
        }
        if width != descriptor.attributes.len() {
            return Err(row_error(
                &target,
                "Input has too many columns.",
                resolver,
                control,
            )?);
        }
        let binding = row_binding(name, &descriptor, implicit, control)?;
        Ok(OwnedScalarExpr::composite_row(
            converted.into_iter(),
            binding,
            Some(crate::type_resolution::base_type(&target).clone_with_control(control)?),
            control,
        )?)
    };
    if domain {
        expression.map_child(0, control, convert)
    } else {
        convert(expression.select_child(0))
    }
}

fn coerce_field(
    item: OwnedScalarExpr,
    source: Option<&ColumnType>,
    target: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    super::super::common::read_unknown_literals_with_control(
        std::iter::once(&*item),
        target,
        control,
    )?;
    if source == Some(target) {
        return Ok(item);
    }
    let name = match target.user_type_identity_with_control(control)? {
        Some(name) => name,
        None => target.catalog_name_with_control(control)?,
    };
    Ok(OwnedScalarExpr::cast(item, name, true, control)?)
}

fn retain_argument_types(
    expression: OwnedScalarExpr,
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::CompositeRow {
        binding: original,
        bound_type,
        ..
    } = &*expression
    else {
        unreachable!();
    };
    if original.argument_types.is_some() {
        return Ok(expression);
    }
    let Some(target) = resolver.resolve_type_name_with_control(&original.ty, control)? else {
        return Err(SQLError::Internal(
            "composite constructor type disappeared".into(),
        ));
    };
    let ColumnType::Composite(reference) = &*target else {
        return Err(SQLError::Internal(
            "composite constructor type disappeared".into(),
        ));
    };
    let descriptor = crate::expr::composites::descriptor_with_control(
        resolver.composite_types(),
        reference.oid,
        control,
    )?;
    let mut attributes = ProductionVec::new(*control);
    let mut types = ProductionVec::new(*control);
    attributes.reserve(original.attributes.len())?;
    types.reserve(original.attributes.len())?;
    for number in &original.attributes {
        attributes.push_copy(*number)?;
        let selected = descriptor
            .attributes
            .iter()
            .find(|attribute| attribute.number == *number)
            .map_or(&ColumnType::Integer, |attribute| &attribute.ty);
        types.push_produced(selected.clone_with_control(control)?)?;
    }
    let binding = binding(
        control.copy_text(&original.ty)?,
        attributes.finish()?,
        types.finish()?,
        original.layout,
        control,
    )?;
    let bound_type = bound_type
        .as_ref()
        .map(|ty| ty.clone_with_control(control))
        .transpose()?;
    Ok(OwnedScalarExpr::composite_row(
        expression.into_children(),
        binding,
        bound_type,
        control,
    )?)
}

pub(crate) fn anonymous_depth(
    expression: &ScalarExpr,
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Option<usize>, SQLError> {
    control.check()?;
    match expression {
        ScalarExpr::Row(_) => Ok(Some(0)),
        ScalarExpr::Cast { expr, ty, .. }
            if resolver
                .resolve_type_name_with_control(ty, control)?
                .as_deref()
                == Some(&ColumnType::Record) =>
        {
            Ok(anonymous_depth(expr, resolver, control)?.map(|depth| depth + 1))
        }
        _ => Ok(None),
    }
}

fn binding(
    name: Produced<String>,
    attributes: Produced<Vec<i16>>,
    types: Produced<Vec<ColumnType>>,
    layout: Option<CompositeRowLayout>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CompositeRowBinding>, SQLError> {
    let (ty, name_memory) = name.into_parts();
    let (attributes, attribute_memory) = attributes.into_parts();
    let (types, type_memory) = types.into_parts();
    Ok(control.finish(
        CompositeRowBinding {
            ty,
            attributes,
            argument_types: Some(types),
            layout,
        },
        control.combine(control.combine(name_memory, attribute_memory), type_memory),
    )?)
}

pub(crate) fn validate_field_cast_with_control(
    target_row: &ColumnType,
    source: Option<&ColumnType>,
    target: &ColumnType,
    position: usize,
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if let Some(source) = source {
        if !super::super::cast_compatibility::explicit_type_compatible_with_control(
            source, target, control,
        )? {
            let source = formatted_type(source, resolver, control)?;
            let target = formatted_type(target, resolver, control)?;
            let detail = control.format(format_args!(
                "Cannot cast type {} to {} in column {position}.",
                source.as_str(),
                target.as_str()
            ))?;
            return Err(row_error(target_row, &detail, resolver, control)?);
        }
    }
    Ok(())
}

fn formatted_type(
    ty: &ColumnType,
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let ty = ty.without_type_modifiers_with_control(control)?;
    resolver.format_type_name_with_control(&ty, control)
}

fn row_error(
    target: &ColumnType,
    detail: &str,
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<SQLError, SQLError> {
    let name = formatted_type(target, resolver, control)?;
    let message = control.format(format_args!("cannot cast type record to {}", name.as_str()))?;
    let detail = control.copy_text(detail)?;
    Ok(SQLError::Diagnostic {
        sqlstate: "42846".into(),
        message: message.into_parts().0,
        detail: Some(detail.into_parts().0),
        hint: None,
    })
}

fn row_binding(
    name: Produced<String>,
    descriptor: &crate::expr::composites::CompositeTypeDescriptor,
    implicit: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<CompositeRowBinding>, SQLError> {
    let mut attributes = ProductionVec::new(*control);
    let mut argument_types = ProductionVec::new(*control);
    attributes.reserve(descriptor.attributes.len())?;
    argument_types.reserve(descriptor.attributes.len())?;
    for attribute in &descriptor.attributes {
        attributes.push_copy(attribute.number)?;
        argument_types.push_produced(attribute.ty.clone_with_control(control)?)?;
    }
    binding(
        name,
        attributes.finish()?,
        argument_types.finish()?,
        Some(CompositeRowLayout {
            width: descriptor.physical_width(),
            implicit,
        }),
        control,
    )
}
