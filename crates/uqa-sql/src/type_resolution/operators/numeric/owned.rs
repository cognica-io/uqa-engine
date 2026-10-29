//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::bind_call_in_place_with_control;
use crate::{
    ast::{FunctionBinding, NumericOperator},
    type_resolution::call,
    SQLError,
};
use uqa_core::memory::{MemoryReservation, ProductionControl};

/// Build selected numeric metadata beside the existing call, then replace only the binding owner. The shared selector fills empty signature/error slots; it never discards an allocating field from this destination.
pub(in crate::type_resolution) fn bind_owned_call(
    operator: NumericOperator,
    call: crate::type_resolution::OwnedBindingCall,
    infer: &mut call::InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<crate::type_resolution::OwnedBindingCall, SQLError> {
    let binding = call.binding().expect("selected numeric operator");
    if !binding.argument_types.is_empty() || binding.resolution_error.is_some() {
        return Ok(call);
    }
    struct Destination {
        binding: FunctionBinding,
        memory: Option<MemoryReservation>,
    }
    let (binding, memory) = binding.clone_with_control(control)?.into_parts();
    let mut destination = Destination { binding, memory };
    bind_call_in_place_with_control(
        operator,
        &mut destination.binding,
        call.arguments(),
        &mut destination.memory,
        infer,
        control,
    )?;
    call.rebind(
        control.finish(destination.binding, destination.memory)?,
        control,
    )
}
