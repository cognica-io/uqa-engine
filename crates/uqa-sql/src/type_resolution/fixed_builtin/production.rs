//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed descriptors and selected bindings retain the same allowance as catalog candidates.

use super::{registry, selection, validate_polymorphic_arguments};
use crate::type_resolution::{
    BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload,
};
use crate::{ast::FunctionBinding, ColumnType, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(in crate::type_resolution) fn overloads_with_control(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<BuiltinFunctionOverload>>>, SQLError> {
    control.check()?;
    let Some((name, signatures)) = registry::lookup(name) else {
        return Ok(None);
    };
    let mut output = ProductionVec::new(*control);
    output.reserve(signatures.len())?;
    for signature in signatures {
        let name = control.format(format_args!("pg_catalog.{name}"))?;
        let mut names = ProductionVec::new(*control);
        names.reserve(signature.argument_types.len())?;
        for index in 0..signature.argument_types.len() {
            let name = signature
                .argument_names
                .get(index)
                .map(|name| control.copy_text(name))
                .transpose()?;
            let (name, memory) = name.map_or((None, control.empty_reservation()), |name| {
                let (name, memory) = name.into_parts();
                (Some(name), memory)
            });
            names.push_produced(control.finish(name, memory)?)?;
        }
        let mut types = ProductionVec::new(*control);
        types.reserve(signature.argument_types.len())?;
        for ty in signature.argument_types {
            types.push_produced(ty.clone_with_control(control)?)?;
        }
        let names = names.finish()?;
        let types = types.finish()?;
        let result = signature.return_type.clone_with_control(control)?;
        let (name, identity_memory) = name.into_parts();
        let (argument_names, parameter_names_memory) = names.into_parts();
        let (argument_types, types_memory) = types.into_parts();
        let (return_type, result_memory) = result.into_parts();
        output.push_produced(control.finish(
            BuiltinFunctionOverload {
                variadic: false,
                name,
                argument_names,
                argument_types,
                default_arguments: signature.default_arguments,
                return_type,
            },
            control.combine(
                control.combine(identity_memory, parameter_names_memory),
                control.combine(types_memory, result_memory),
            ),
        )?)?;
    }
    Ok(Some(output.finish()?))
}

pub(in crate::type_resolution) fn resolve_overload_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ResolvedFunctionOverload>, SQLError> {
    crate::expr::validate_named_argument_order_with_control(
        argument_names.iter().map(Option::as_deref),
        control,
    )?;
    if let Some(resolver) = resolver {
        let builtins = overloads_with_control(name, control)?.ok_or_else(|| {
            crate::type_resolution::function_resolution_error(
                "42883",
                name,
                argument_names,
                argument_types,
                "does not exist",
            )
        })?;
        if let Some(selected) = resolver.resolve_function_overload_with_builtins_and_control(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            &builtins,
            control,
        )? {
            if let Some(overload) = builtins.iter().find(|overload| {
                selected.binding.builtin
                    && overload.argument_types.contains(&ColumnType::AnyArray)
                    && crate::type_resolution::builtin_binding_matches(overload, &selected.binding)
            }) {
                if let Some(matched) =
                    crate::type_resolution::overload_resolution::match_signature_with_control(
                        overload,
                        argument_names,
                        argument_types,
                        control,
                    )?
                {
                    validate_polymorphic_arguments(
                        &overload.argument_types,
                        argument_types,
                        &matched.argument_positions,
                    )?;
                }
            }
            return Ok(selected);
        }
    }
    selection::resolve_overload_with_control(
        name,
        binding,
        argument_names,
        argument_types,
        explicit_variadic,
        control,
    )
}
