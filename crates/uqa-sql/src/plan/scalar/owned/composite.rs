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
        self.replace_text_field(name, control, |expression| {
            expression
                .written_type_name_mut()
                .expect("selected type name requires a typed constructor")
        })
    }
}
