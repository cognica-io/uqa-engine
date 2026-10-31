//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    type_resolution::{call::InferType, OwnedBindingCall},
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub(in crate::type_resolution) fn bind_owned_call(
    call: OwnedBindingCall,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<OwnedBindingCall, SQLError> {
    match super::select_binding(
        call.name(),
        call.binding(),
        call.arguments(),
        infer,
        control,
    )? {
        Some(selected) => call.rebind(selected, control),
        None => Ok(call),
    }
}
