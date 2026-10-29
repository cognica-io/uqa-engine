//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected containment casts transfer their existing argument owner into an admitted wrapper.

use crate::{
    plan::OwnedScalarExpr,
    type_resolution::{call::InferType, OwnedBindingCall},
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub(in crate::type_resolution) fn bind_owned_call(
    call: OwnedBindingCall,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<OwnedBindingCall, SQLError> {
    let Some(prepared) = super::select_cast(call.arguments(), infer, control)? else {
        return Ok(call);
    };
    let ((index, name), memory) = prepared.into_parts();
    let name = control.finish(name, memory)?;
    call.map_argument(index, control, |argument| {
        Ok(OwnedScalarExpr::cast(argument, name, true, control)?)
    })
}
