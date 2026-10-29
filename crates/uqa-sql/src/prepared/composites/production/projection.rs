//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    current_descriptors, DescriptorSet, FunctionTypeResolver, MutableExecutablePlan,
    ProductionControl, SQLError, ScalarExpr, UnifiedPlan,
};
use crate::{
    expr::composites::{
        retained::production::project_value_with_control, CompositeConstantSource,
        CompositeTypeDescriptor,
    },
    plan::OwnedScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionVec},
    Value,
};

pub(super) fn project_plan(
    plan: &UnifiedPlan,
    types: &dyn FunctionTypeResolver,
    original: &DescriptorSet,
    interpret: bool,
    binding_scope: Option<&DescriptorSet>,
    control: &ProductionControl<'_>,
) -> Result<Option<MutableExecutablePlan>, SQLError> {
    control.check()?;
    if original.is_empty() {
        return Ok(None);
    }
    let current = current_descriptors(original, types, control)?;
    let values_changed = *original != current;
    let bindings_changed = binding_scope.is_some_and(|scope| {
        current
            .iter()
            .any(|(oid, descriptor)| scope.get(*oid) != Some(descriptor))
    });
    if !values_changed && !bindings_changed {
        return Ok(None);
    }
    let plan =
        plan.copy_for_rewrite(control)?
            .transform_scalar_nodes(control, &mut |_, node| {
                let node = if values_changed {
                    project_literal(node, types, original, interpret, control)?
                } else {
                    node
                };
                project_field(node, &current, control)
            })?;
    Ok(Some(plan))
}

fn project_literal(
    node: OwnedScalarExpr,
    types: &dyn FunctionTypeResolver,
    original: &DescriptorSet,
    interpret: bool,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::TypedLiteral {
        value,
        ty,
        parameter_index: None,
        bound_type,
        composite_source,
        ..
    } = &*node
    else {
        return Ok(node);
    };
    if !crate::expr::composites::literal::contains_records(value) {
        return Ok(node);
    }
    let selected = if bound_type.is_none() {
        types.resolve_type_name_with_control(ty, control)?
    } else {
        None
    };
    let Some(ty) = bound_type.as_ref().or(selected.as_deref()) else {
        return Ok(node);
    };
    let source = capture_source(value, composite_source.as_deref(), original, types, control)?;
    let descriptors = DescriptorSet::from_descriptors(&source.descriptors, control)?;
    let live = current_descriptors(&descriptors, types, control)?;
    let value =
        project_value_with_control(&source.value, ty, &descriptors, &live, interpret, control)?;
    Ok(node.replace_composite_literal(value, source, control)?)
}

fn capture_source(
    value: &Value,
    source: Option<&CompositeConstantSource>,
    original: &DescriptorSet,
    types: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Produced<CompositeConstantSource>, SQLError> {
    let value = crate::expr::enums::retain_enum_oids_with_control(
        types.enum_labels(),
        source.map_or(value, |source| &source.value),
        control,
    )?;
    let mut descriptors = ProductionVec::new(*control);
    if let Some(source) = source {
        for descriptor in &source.descriptors {
            descriptors.push_produced(copy_descriptor(descriptor, control)?)?;
        }
    } else {
        for (_, descriptor) in original.iter() {
            descriptors.push_produced(copy_descriptor(descriptor, control)?)?;
        }
    }
    let (descriptors, descriptor_memory) = descriptors.finish()?.into_parts();
    let (value, value_memory) = value.into_parts();
    Ok(control.finish(
        CompositeConstantSource { value, descriptors },
        control.combine(value_memory, descriptor_memory),
    )?)
}

fn copy_descriptor(
    descriptor: &CompositeTypeDescriptor,
    control: &ProductionControl<'_>,
) -> Result<Produced<CompositeTypeDescriptor>, SQLError> {
    CompositeTypeDescriptor::from_metadata_with_control(
        descriptor.type_oid,
        descriptor.relation_oid,
        descriptor.attributes.iter().map(|attribute| {
            Ok((
                attribute.name.as_str(),
                &attribute.ty,
                attribute.number,
                attribute.collation_oid,
            ))
        }),
        descriptor.dropped.iter().copied().map(Ok),
        control,
    )
}

fn project_field(
    node: OwnedScalarExpr,
    current: &DescriptorSet,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = &*node
    else {
        return Ok(node);
    };
    let Some(field) = binding.composite_field() else {
        return Ok(node);
    };
    let Some(descriptor) = current.get(field.type_oid) else {
        return Ok(node);
    };
    let attribute = descriptor
        .attributes
        .iter()
        .find(|attribute| attribute.number == field.number);
    let changed = attribute
        .filter(|attribute| {
            crate::catalog::type_metadata::pg_type_oid(&attribute.ty)
                != crate::catalog::type_metadata::pg_type_oid(&field.result_type)
        })
        .map(|attribute| &attribute.ty);
    let binding =
        binding.with_composite_projection_with_control(attribute.is_none(), changed, control)?;
    let replacement = if let (Some(attribute), Some(ScalarExpr::Literal(Value::Str(_)))) =
        (attribute, args.get(1))
    {
        let (name, memory) = control.copy_text(&attribute.name)?.into_parts();
        Some(OwnedScalarExpr::literal(
            control.finish(Value::Str(name), memory)?,
            control,
        )?)
    } else {
        None
    };
    let node = node.replace_call_binding(Some(binding), control)?;
    Ok(if let Some(replacement) = replacement {
        node.replace_child(1, replacement, control)?.0
    } else {
        node
    })
}
