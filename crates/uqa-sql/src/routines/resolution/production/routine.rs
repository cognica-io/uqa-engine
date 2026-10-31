//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedure and function calls share admitted candidate ranking while retaining their selected invocation independently.

use super::{diagnostics, matching, selection};
use crate::{
    ast::{FunctionBinding, RoutineInvocationBinding, RoutineVariadicMode},
    routines::{
        resolution::{RoutineCallKind, RoutineOverloadContext},
        SQLUserFunction,
    },
    type_resolution::RoutineCallDescriptor,
    ColumnType, SQLError,
};
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

pub struct OwnedStaticFunctionMatch {
    pub function: Arc<SQLUserFunction>,
    pub invocation: Produced<RoutineInvocationBinding>,
}

impl RoutineOverloadContext<'_> {
    #[expect(
        clippy::too_many_arguments,
        reason = "selected call syntax, routine kind and admission share one existing resolution boundary"
    )]
    pub fn resolve_static_sql_routine_match_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        kind: RoutineCallKind,
        control: &ProductionControl<'_>,
    ) -> Result<Option<OwnedStaticFunctionMatch>, SQLError> {
        control.check()?;
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
                return Ok(Some(OwnedStaticFunctionMatch {
                    function,
                    invocation: crate::plan::copy::copy_syntax_with_control(
                        invocation.as_ref(),
                        control,
                    )?,
                }));
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
            let Some(matched) = selection::select_unbound_kind(self, name, call, kind, control)?
            else {
                return Ok(None);
            };
            matched
        };
        diagnostics::kind(name, call, kind, &matched.function.def, control)?;
        if let Some(error) = &matched.signature.default_error {
            return Err(diagnostics::default_type(self.catalog, error, control)?);
        }
        Ok(Some(OwnedStaticFunctionMatch {
            function: matched.function,
            invocation: matched.invocation,
        }))
    }
}
