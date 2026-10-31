//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected routine metadata borrows immutable declarations and admits its independent output.

use super::{RoutineCallKind, RoutineOverloadContext};
use crate::{
    ast::{
        ColumnType, CreateFunction, FunctionBinding, FunctionReturns, RoutineInvocationBinding,
        RoutineVariadicMode,
    },
    type_resolution::{
        canonical_routine_type_name_with_control, FunctionBindingParts, ResolvedFunctionOverload,
        RoutineSignatureMatchError,
    },
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity, RelationName,
};

pub(super) mod combined;
pub(super) mod diagnostics;
mod invocation;
mod matching;
mod parameters;
mod routine;
pub(super) mod selection;
mod table;
pub use routine::OwnedStaticFunctionMatch;

#[cfg(test)]
mod tests;

/// Observe the original first-occurrence search-path order, including duplicate entries.
pub fn search_path_precedes_pg_catalog<'a>(
    path: impl IntoIterator<Item = &'a str>,
    schema: RelationName<'_>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut user_position = None;
    let mut catalog_position = None;
    for (position, entry) in path.into_iter().enumerate() {
        control.check()?;
        if user_position.is_none() && RelationName::decoded(entry) == schema {
            user_position = Some(position);
        }
        if catalog_position.is_none() && entry == "pg_catalog" {
            catalog_position = Some(position);
        }
        if let (Some(user), Some(catalog)) = (user_position, catalog_position) {
            return Ok(user < catalog);
        }
    }
    Ok(false)
}

impl RoutineOverloadContext<'_> {
    pub(super) fn user_function_precedes_pg_catalog_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let Some(schema) = RelationIdentity::borrowed_reference(name).and_then(|name| name.schema)
        else {
            return Ok(false);
        };
        self.catalog
            .routine_schema_precedes_pg_catalog(schema, control)
    }

    /// Revalidate the selected identity and invocation, then construct exactly the binding and result type the caller retains.
    pub fn resolve_bound_function_overload_with_control(
        &self,
        name: &str,
        binding: &FunctionBinding,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ResolvedFunctionOverload>, SQLError> {
        control.check()?;
        let kind = RoutineCallKind::Function;
        let function = self
            .catalog
            .select_bound_routine_with_control(binding, kind, control)?
            .ok_or_else(|| {
                diagnostics::bound(kind, binding, control).unwrap_or_else(std::convert::identity)
            })?;
        let invocation = binding.invocation.as_deref().ok_or_else(|| {
            diagnostics::bound(kind, binding, control).unwrap_or_else(std::convert::identity)
        })?;
        let invocation_is_explicit = matches!(
            invocation.variadic_mode,
            RoutineVariadicMode::Explicit { .. }
        );
        if invocation.argument_positions.len() != argument_types.len()
            || invocation_is_explicit != explicit_variadic
        {
            return Err(
                diagnostics::bound(kind, binding, control).unwrap_or_else(std::convert::identity)
            );
        }
        diagnostics::kind(
            name,
            crate::type_resolution::RoutineCallDescriptor {
                argument_names,
                argument_types,
                explicit_variadic,
            },
            kind,
            &function.def,
            control,
        )?;
        check_invocation_result(self, invocation, control)?;
        let selected = FunctionBindingParts {
            object_id: function.def.object_id,
            name: control.copy_text(&function.def.name)?,
            argument_types: crate::plan::copy::copy_syntax_with_control(
                &binding.argument_types,
                control,
            )?,
            builtin: false,
            dispatch: None,
            invocation: Some(crate::plan::copy::copy_syntax_with_control(
                invocation, control,
            )?),
            value_binding: None,
            resolution_error: None,
            input_collation: None,
        }
        .finish(control)?;
        let result = static_function_return_type_with_control(
            self,
            name,
            &function.def,
            Some(invocation),
            control,
        )?;
        let precedes_pg_catalog =
            self.user_function_precedes_pg_catalog_with_control(&function.def.name, control)?;
        let (binding, binding_memory) = selected.into_parts();
        let (return_type, return_memory) = result.into_parts();
        Ok(control.finish(
            ResolvedFunctionOverload {
                binding,
                return_type,
                exact_matches: 0,
                known_arguments: argument_types.iter().flatten().count(),
                preferred_matches: 0,
                precedes_pg_catalog,
            },
            control.combine(binding_memory, return_memory),
        )?)
    }
}

fn check_invocation_result(
    resolver: &RoutineOverloadContext<'_>,
    invocation: &RoutineInvocationBinding,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for ty in &invocation.parameter_types {
        control.check()?;
        if ty != "anyarray" {
            continue;
        }
        if let Some(result) = &invocation.return_type {
            let canonical = canonical_routine_type_name_with_control(result, control)?;
            if matches!(canonical.as_str(), "anyelement" | "anynonarray" | "anyenum") {
                return Err(diagnostics::default_type(
                    resolver.catalog,
                    &RoutineSignatureMatchError::IndeterminateArrayElement,
                    control,
                )?);
            }
        }
        break;
    }
    Ok(())
}

pub(super) fn static_function_return_type_with_control(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    definition: &CreateFunction,
    invocation: Option<&RoutineInvocationBinding>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    control.check()?;
    let invocation_return = invocation.and_then(|invocation| invocation.return_type.as_deref());
    let declared_return = match &definition.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            Some(type_name.as_str())
        }
        FunctionReturns::Table | FunctionReturns::None => None,
    };
    if let Some(type_name) = invocation_return.or(declared_return) {
        if canonical_routine_type_name_with_control(type_name, control)?.as_str() == "trigger" {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "trigger functions can only be called as triggers".into(),
            });
        }
    }
    if matches!(definition.returns, FunctionReturns::Table)
        || definition.output_params().nth(1).is_some()
    {
        return Ok(control.finish(ColumnType::Record, control.empty_reservation())?);
    }
    if let Some(type_name) = invocation_return {
        return resolve_return_type(resolver, type_name, control)?.ok_or_else(|| {
            return_type_error(
                format_args!("function `{name}` has unresolved return type `{type_name}`"),
                control,
            )
        });
    }
    let type_name = match &definition.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => type_name,
        FunctionReturns::None => {
            &definition
                .output_params()
                .next()
                .ok_or_else(|| {
                    return_type_error(
                        format_args!("function `{name}` does not return a value"),
                        control,
                    )
                })?
                .type_name
        }
        FunctionReturns::Table => unreachable!("table result handled above"),
    };
    resolve_return_type(resolver, type_name, control)?
        .ok_or_else(|| return_type_error(format_args!("unknown type `{type_name}`"), control))
}

fn return_type_error(
    message: std::fmt::Arguments<'_>,
    control: &ProductionControl<'_>,
) -> SQLError {
    match control.format(message) {
        Ok(message) => {
            let (message, _memory) = message.into_parts();
            SQLError::TypeMismatch(message)
        }
        Err(error) => error.into(),
    }
}

fn resolve_return_type(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    if let Some(ty) = resolver
        .catalog
        .resolve_catalog_column_type_with_control(name, control)?
    {
        return Ok(Some(ty));
    }
    match ColumnType::from_sql_name_with_control(name, control) {
        Ok(ty) => Ok(Some(ty)),
        Err(error)
            if matches!(error.primary(), SQLError::Cancelled(_))
                || error.sqlstate() == Some("53200") =>
        {
            Err(error)
        }
        Err(_) => Ok(None),
    }
}

pub(super) fn match_ordinary(
    catalog: &super::RoutineTypeSnapshot,
    function: std::sync::Arc<crate::routines::SQLUserFunction>,
    call: crate::type_resolution::RoutineCallDescriptor<'_>,
    kind: RoutineCallKind,
) -> Result<Option<crate::routines::StaticFunctionMatch>, RoutineSignatureMatchError> {
    let control = ProductionControl::uncontrolled();
    match matching::match_candidate(catalog, function, call, kind, &control) {
        Ok(matched) => Ok(matched.map(|matched| {
            let signature = matched
                .signature
                .into_uncontrolled()
                .expect("ordinary matched routine");
            let invocation = matched
                .invocation
                .into_uncontrolled()
                .expect("ordinary invocation metadata");
            crate::routines::StaticFunctionMatch {
                function: matched.function,
                invocation: Box::new(invocation),
                argument_types: signature.argument_signature,
                raw_exact_matches: signature.raw_exact_matches,
                exact_matches: signature.exact_matches,
                preferred_matches: signature.preferred_matches,
                variadic_expansion: signature.variadic_mode
                    == crate::type_resolution::RoutineVariadicMode::Pack,
                default_error: signature.default_error,
            }
        })),
        Err(crate::type_resolution::RoutineSignatureProductionError::Signature(error)) => {
            Err(error
                .into_uncontrolled()
                .expect("ordinary signature diagnostic"))
        }
        Err(crate::type_resolution::RoutineSignatureProductionError::Production(error)) => {
            panic!("ordinary routine matching cannot fail production: {error}")
        }
    }
}

pub(crate) fn selected_binding(
    function: &crate::routines::SQLUserFunction,
    invocation: Produced<RoutineInvocationBinding>,
    control: &ProductionControl<'_>,
) -> Result<Produced<FunctionBinding>, SQLError> {
    let mut arguments = uqa_core::memory::ProductionVec::new(*control);
    for parameter in function.def.identity_params() {
        control.check()?;
        arguments.push_produced(canonical_routine_type_name_with_control(
            &parameter.type_name,
            control,
        )?)?;
    }
    FunctionBindingParts {
        object_id: function.def.object_id,
        name: control.copy_text(&function.def.name)?,
        argument_types: arguments.finish()?,
        builtin: false,
        dispatch: None,
        invocation: Some(invocation),
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    }
    .finish(control)
}
