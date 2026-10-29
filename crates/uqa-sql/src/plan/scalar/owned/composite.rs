//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named row constructors keep their admitted metadata independent from the scalar fields they consume.

use super::{
    generated::{finish, owned_lowering},
    OwnedScalarExpr, Result, ScalarExpr,
};
use crate::{ast::CompositeRowBinding, ColumnType};
use uqa_core::memory::{Produced, ProductionControl};

impl OwnedScalarExpr {
    pub(crate) fn composite_row(
        children: impl ExactSizeIterator<Item = Self>,
        binding: Produced<CompositeRowBinding>,
        bound_type: Option<Produced<ColumnType>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let mut lowering = owned_lowering(control);
        let (binding, binding_memory) = binding.into_parts();
        let (bound_type, type_memory) = match bound_type {
            Some(ty) => {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        let memory = control.combine(binding_memory, type_memory);
        if let Some(owner) = &mut lowering.control {
            owner
                .memory
                .absorb(memory.expect("admitted named row metadata"));
        }
        let items = lowering.map(children, Self::attach_child)?;
        finish(
            ScalarExpr::CompositeRow {
                items,
                binding,
                bound_type,
            },
            lowering,
            control,
        )
    }

    /// Replace only the selected written type while the scalar constructor preserves its children and every other field.
    pub(crate) fn replace_type_name(
        self,
        name: Produced<String>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        match &*self {
            ScalarExpr::Cast { implicit, .. } => {
                let implicit = *implicit;
                Self::cast(self.select_child(0), name, implicit, control)
            }
            ScalarExpr::TypedLiteral {
                value,
                bound_type,
                parameter_index,
                composite_source,
                collation_oid,
                ..
            } => {
                let mut lowering = owned_lowering(control);
                use crate::plan::scalar::source::Source;
                let value = lowering.value(Source::Borrowed(value))?;
                let bound_type = lowering.scalar_type_copy(bound_type.as_ref())?;
                let composite_source =
                    lowering.composite_source(composite_source.as_ref().map(Source::Borrowed))?;
                let parameter_index = *parameter_index;
                let collation_oid = *collation_oid;
                let (ty, memory) = name.into_parts();
                if let Some(owner) = &mut lowering.control {
                    owner
                        .memory
                        .absorb(memory.expect("admitted literal type name"));
                }
                finish(
                    ScalarExpr::TypedLiteral {
                        value,
                        ty,
                        bound_type,
                        parameter_index,
                        composite_source,
                        collation_oid,
                    },
                    lowering,
                    control,
                )
            }
            ScalarExpr::CompositeRow {
                binding,
                bound_type,
                ..
            } => {
                let binding = binding_with_name(binding, name, control)?;
                let bound_type = bound_type
                    .as_ref()
                    .map(|ty| ty.clone_with_control(control))
                    .transpose()?;
                Self::composite_row(self.into_children(), binding, bound_type, control)
            }
            _ => panic!("selected type name requires a typed scalar constructor"),
        }
    }
}

pub(crate) fn binding_with_name(
    binding: &CompositeRowBinding,
    name: Produced<String>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CompositeRowBinding>> {
    use uqa_core::memory::ProductionVec;
    let mut attributes = ProductionVec::new(*control);
    attributes.reserve(binding.attributes.len())?;
    for number in &binding.attributes {
        attributes.push_copy(*number)?;
    }
    let argument_types = binding
        .argument_types
        .as_ref()
        .map(|types| {
            let mut output = ProductionVec::new(*control);
            output.reserve(types.len())?;
            for ty in types {
                output.push_produced(ty.clone_with_control(control)?)?;
            }
            output.finish()
        })
        .transpose()?;
    let (ty, mut memory) = name.into_parts();
    let (attributes, retained) = attributes.finish()?.into_parts();
    memory = control.combine(memory, retained);
    let argument_types = argument_types.map(|types| {
        let (types, retained) = types.into_parts();
        memory = control.combine(memory.take(), retained);
        types
    });
    Ok(control.finish(
        CompositeRowBinding {
            ty,
            attributes,
            argument_types,
            layout: binding.layout,
        },
        memory,
    )?)
}
