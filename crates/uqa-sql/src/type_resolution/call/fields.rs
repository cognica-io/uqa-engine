//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function binding publication consumes already admitted field owners.

use crate::{
    ast::{
        FunctionBinding, FunctionDispatch, FunctionResolutionError, FunctionValueBinding,
        RoutineInvocationBinding,
    },
    catalog::collations::CollationSelection,
    SQLError,
};
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

/// Constructor inputs for a selected call's immutable binding. Allocating fields keep their original producer leases until this owner publishes or destroys the complete binding.
#[derive(Debug)]
pub struct FunctionBindingParts {
    pub object_id: Option<[u8; 16]>,
    pub name: Produced<String>,
    pub argument_types: Produced<Vec<String>>,
    pub builtin: bool,
    pub dispatch: Option<FunctionDispatch>,
    pub invocation: Option<Produced<RoutineInvocationBinding>>,
    pub value_binding: Option<Produced<FunctionValueBinding>>,
    pub resolution_error: Option<Produced<FunctionResolutionError>>,
    pub input_collation: Option<CollationSelection>,
}

impl FunctionBindingParts {
    /// Start a parser-selected binding with independently owned name and signature fields.
    pub fn dispatched(
        dispatch: FunctionDispatch,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            object_id: None,
            name: control.copy_text(dispatch.label())?,
            argument_types: control.finish(Vec::new(), control.empty_reservation())?,
            builtin: true,
            dispatch: Some(dispatch),
            invocation: None,
            value_binding: None,
            resolution_error: None,
            input_collation: None,
        })
    }

    /// Admit optional field boxes before allocation and transfer every completed field's existing reservation into the published binding.
    pub fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<FunctionBinding>, SQLError> {
        control.check()?;
        let bytes = usize::from(self.invocation.is_some()) * size_of::<RoutineInvocationBinding>()
            + usize::from(self.value_binding.is_some()) * size_of::<FunctionValueBinding>()
            + usize::from(self.resolution_error.is_some()) * size_of::<FunctionResolutionError>();
        let mut memory = control.reserve(bytes)?;
        let binding = FunctionBinding {
            object_id: self.object_id,
            name: take(self.name, &mut memory, control),
            argument_types: take(self.argument_types, &mut memory, control),
            builtin: self.builtin,
            dispatch: self.dispatch,
            invocation: self
                .invocation
                .map(|value| Box::new(take(value, &mut memory, control))),
            value_binding: self
                .value_binding
                .map(|value| Box::new(take(value, &mut memory, control))),
            resolution_error: self
                .resolution_error
                .map(|value| Box::new(take(value, &mut memory, control))),
            input_collation: self.input_collation,
        };
        Ok(control.finish(binding, memory)?)
    }
}

fn take<T>(
    value: Produced<T>,
    memory: &mut Option<MemoryReservation>,
    control: &ProductionControl<'_>,
) -> T {
    let (value, retained) = value.into_parts();
    *memory = control.combine(memory.take(), retained);
    value
}
