//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional builtin and catalog producers run in their established order under one scalar owner's resource scope.

use super::{OwnedBinder, OwnedBindingCall, SQLError};

impl OwnedBinder<'_> {
    pub(super) fn bind_optional_calls(
        &self,
        call: OwnedBindingCall,
    ) -> Result<OwnedBindingCall, SQLError> {
        let control = &self.context.control;
        let params = self.context.params;
        let resolver = self.context.resolver;
        let mut infer = |expression: &crate::ScalarExpr| self.context.infer(expression);
        let call = crate::type_resolution::json_path::bind_owned_call(
            call, params, &mut infer, resolver, control,
        )?;
        let call = crate::type_resolution::array_transform::bind_owned_call(
            call, params, &mut infer, resolver, control,
        )?;
        let call = crate::type_resolution::range::bind_owned_call(call, &mut infer, control)?;
        let call = crate::type_resolution::fixed_builtin::bind_owned_call(
            call, params, &mut infer, resolver, control,
        )?;
        self.bind_catalog_function(call)
    }

    fn bind_catalog_function(&self, call: OwnedBindingCall) -> Result<OwnedBindingCall, SQLError> {
        if call.binding().is_some() {
            return Ok(call);
        }
        if crate::ast::is_builtin_aggregate_function(&self.local_name(call.name())?) {
            return Ok(call);
        }
        let control = &self.context.control;
        let builtin = crate::type_resolution::functions::builtin_function_type_with_control(
            crate::type_resolution::functions::FunctionTypeCall {
                name: call.name(),
                binding: None,
                args: call.arguments(),
            },
            &[],
            self.context.params,
            None,
            &mut |expression| {
                crate::type_resolution::scalar_type_inner_with_control(
                    expression,
                    self.context.schema,
                    self.context.params,
                    None,
                    control,
                )
            },
            control,
        );
        if self.context.semantic(builtin)?.flatten().is_some() {
            return Ok(call);
        }
        let Some(resolver) = self.context.resolver else {
            return Ok(call);
        };
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
            return Ok(call);
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
        let Some(selected) = selected else {
            return Ok(call);
        };
        if !self
            .context
            .semantic(resolver.is_scalar_function_binding_with_control(&selected.binding, control))?
            .unwrap_or(false)
        {
            return Ok(call);
        }
        let binding = selected.binding.clone_with_control(control)?;
        drop(selected);
        call.rebind(binding, control)
    }
}
