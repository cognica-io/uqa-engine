//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concrete enum calls keep the selected metadata alive through overload checks and unknown-input coercions.

use super::{ColumnType, FunctionDispatch, OwnedBinder, OwnedBindingCall, SQLError};
use crate::{
    ast::{EnumFunctionOperation, FunctionBinding},
    type_resolution::enums::{enum_argument_type, EnumArgumentType},
};
use uqa_core::memory::ProductionVec;

impl OwnedBinder<'_> {
    pub(super) fn bind_enum_call(
        &self,
        mut call: OwnedBindingCall,
    ) -> Result<OwnedBindingCall, SQLError> {
        if call.binding().is_some() {
            return Ok(call);
        }
        let Some(operation) = EnumFunctionOperation::from_call(call.name(), call.arguments().len())
        else {
            return Ok(call);
        };
        let control = &self.context.control;
        let mut types = ProductionVec::new(*control);
        types.reserve(call.arguments().len())?;
        for argument in call.arguments() {
            if crate::scalar_call_argument(argument).is_ok_and(|argument| argument.name.is_some()) {
                return Ok(call);
            }
            let ty = self
                .context
                .semantic(self.context.common_context(argument))?
                .flatten();
            let (ty, memory) = match ty {
                Some(ty) => {
                    let (ty, memory) = ty.into_parts();
                    (Some(ty), memory)
                }
                None => (None, control.empty_reservation()),
            };
            types.push_produced(control.finish(ty, memory)?)?;
        }
        let types = types.finish()?;
        let mut borrowed = ProductionVec::new(*control);
        borrowed.reserve(types.len())?;
        for ty in types.iter() {
            borrowed.push_copy(ty.as_ref())?;
        }
        let EnumArgumentType::Enum(reference) = enum_argument_type(operation, &borrowed) else {
            return Ok(call);
        };
        let target = crate::type_resolution::enums::result_type_with_control(
            EnumFunctionOperation::First,
            reference,
            control,
        )?;
        if self.user_routine_matches_exactly(&call)? {
            return Ok(call);
        }
        let binding = FunctionBinding::dispatched_with_control(
            FunctionDispatch::Enum {
                operation,
                type_oid: reference.oid,
            },
            control,
        )?;
        call = call.rebind(binding, control)?;
        for (index, ty) in types.iter().enumerate() {
            if ty.is_some() {
                continue;
            }
            let target = if index < operation.enum_argument_count() {
                &*target
            } else {
                &ColumnType::BigInteger
            };
            call = call.map_argument(index, control, |argument| {
                self.common_cast(argument, target)
            })?;
        }
        Ok(call)
    }

    fn user_routine_matches_exactly(&self, call: &OwnedBindingCall) -> Result<bool, SQLError> {
        let Some(resolver) = self.context.resolver else {
            return Ok(false);
        };
        let control = &self.context.control;
        let Some(signature) = self.context.semantic(
            crate::type_resolution::common::function_call_argument_signature_with_control(
                call.arguments(),
                self.context.schema,
                self.context.params,
                Some(resolver),
                control,
            ),
        )?
        else {
            return Ok(false);
        };
        let (names, types, variadic) = &*signature;
        let selected = self
            .context
            .semantic(resolver.resolve_function_overload_with_control(
                call.name(),
                None,
                names,
                types,
                *variadic,
                control,
            ))?
            .flatten();
        Ok(selected.is_some_and(|selected| {
            !selected.binding.builtin && selected.exact_matches == selected.known_arguments
        }))
    }
}
