//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table calls share candidate selection and retain the selected invocation independently.

use super::{
    check_invocation_result, combined, diagnostics, matching, selected_binding, selection,
};
use crate::{
    ast::{ColumnType, FunctionBinding, RoutineVariadicMode},
    plan::copy::copy_syntax_with_control,
    routines::resolution::{RoutineCallKind, RoutineOverloadContext},
    type_resolution::{BuiltinFunctionOverload, ResolvedFunctionOverload, RoutineCallDescriptor},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

impl RoutineOverloadContext<'_> {
    /// Static table binding checks the selected routine without resolving a scalar output type.
    pub fn resolve_static_sql_function_binding_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<FunctionBinding>>, SQLError> {
        control.check()?;
        let kind = RoutineCallKind::Function;
        let call = RoutineCallDescriptor {
            argument_names,
            argument_types,
            explicit_variadic,
        };
        let matched = if let Some(binding) = binding {
            if binding.builtin {
                return Ok(None);
            }
            let function = self
                .catalog
                .select_bound_routine_with_control(binding, kind, control)?
                .ok_or_else(|| {
                    diagnostics::bound(kind, binding, control)
                        .unwrap_or_else(std::convert::identity)
                })?;
            if let Some(invocation) = &binding.invocation {
                if invocation.argument_positions.len() != argument_types.len()
                    || matches!(
                        invocation.variadic_mode,
                        RoutineVariadicMode::Explicit { .. }
                    ) != explicit_variadic
                {
                    return Err(diagnostics::bound(kind, binding, control)?);
                }
                diagnostics::kind(name, call, kind, &function.def, control)?;
                check_invocation_result(self, invocation, control)?;
                return selected_binding(
                    &function,
                    copy_syntax_with_control(invocation.as_ref(), control)?,
                    control,
                )
                .map(Some);
            }
            matching::match_candidate(
                &self.catalog.routine_type_snapshot(),
                function,
                call,
                kind,
                control,
            )
            .map_err(|error| selection::signature_error(error, control))?
            .ok_or_else(|| {
                diagnostics::bound(kind, binding, control).unwrap_or_else(std::convert::identity)
            })?
        } else {
            let Some(matched) = selection::select_unbound(self, name, call, control)? else {
                return Ok(None);
            };
            matched
        };
        diagnostics::kind(name, call, kind, &matched.function.def, control)?;
        if let Some(error) = &matched.signature.default_error {
            return Err(diagnostics::default_type(self.catalog, error, control)?);
        }
        check_invocation_result(self, &matched.invocation, control)?;
        selected_binding(&matched.function, matched.invocation, control).map(Some)
    }

    pub fn resolve_table_function_overload_with_builtins_and_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        call: crate::type_resolution::RoutineCallDescriptor<'_>,
        builtins: &[BuiltinFunctionOverload],
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ResolvedFunctionOverload>>, SQLError> {
        let selected = combined::resolve(self, name, binding, call, builtins, control)?;
        let Some(binding) = binding.filter(|binding| !binding.builtin) else {
            return Ok(Some(selected));
        };
        // Stored table calls keep their original binding; scalar selection may regenerate it.
        let binding = copy_syntax_with_control(binding, control)?;
        let return_type = selected.return_type.clone_with_control(control)?;
        let known_arguments = selected.known_arguments;
        let precedes_pg_catalog = selected.precedes_pg_catalog;
        drop(selected);
        let (binding, binding_memory) = binding.into_parts();
        let (return_type, type_memory) = return_type.into_parts();
        Ok(Some(control.finish(
            ResolvedFunctionOverload {
                binding,
                return_type,
                exact_matches: 0,
                known_arguments,
                preferred_matches: 0,
                precedes_pg_catalog,
            },
            control.combine(binding_memory, type_memory),
        )?))
    }
}
