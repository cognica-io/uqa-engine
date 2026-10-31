//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM-call selection owns candidate metadata and preserves catalog callback order.

use super::{
    has_builtin_table_function_overloads, is_builtin_table_function, BuiltinFunctionOverload,
    ColumnType, FunctionBinding, RoutineResolution, SQLError,
};
use crate::type_resolution::RoutineCallDescriptor;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(crate) fn resolve_table_function_binding_for_types_with_control(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    call: RoutineCallDescriptor<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<FunctionBinding>>, SQLError> {
    control.check()?;
    if let Some(binding) = binding {
        return Ok(Some(binding.clone_with_control(control)?));
    }
    let identity = crate::semantics::lowercase_function_name_with_control(name, control)?;
    let builtin =
        crate::semantics::builtin_function_dispatch_name_with_control(&identity, control)?;
    let builtins =
        builtin_table_function_overloads_with_control(&builtin, call.argument_types, control)?;
    if !builtins.is_empty() || has_builtin_table_function_overloads(&builtin) {
        return routines
            .resolve_table_function_overload_with_builtins_and_control(
                name, None, call, &builtins, control,
            )?
            .map(|resolved| resolved.binding.clone_with_control(control))
            .transpose()
            .map_err(Into::into);
    }
    let builtin_surface = is_builtin_table_function(&builtin)
        || crate::registry::is_operator_join_table_function(&builtin)
        || routines.has_registered_table_function_with_control(&identity, control)?;
    if !routines.has_visible_sql_functions_with_control(name, control)? {
        return Ok(None);
    }
    match routines.resolve_static_sql_function_binding_with_control(
        name,
        None,
        call.argument_names,
        call.argument_types,
        call.explicit_variadic,
        control,
    ) {
        Err(error) if builtin_surface && error.sqlstate() == Some("42883") => Ok(None),
        result => result,
    }
}

pub(super) fn builtin_table_function_overloads_with_control(
    name: &str,
    arguments: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<BuiltinFunctionOverload>>, SQLError> {
    control.check()?;
    let mut output = ProductionVec::new(*control);
    if matches!(
        name,
        "generate_series" | "regexp_matches" | "regexp_split_to_table"
    ) {
        let declarations =
            crate::type_resolution::fixed_builtin_overloads_with_control(name, control)?
                .expect("set functions have fixed catalog signatures");
        output.reserve(declarations.len())?;
        for declaration in declarations.iter().filter(|declaration| {
            name != "generate_series"
                || matches!(
                    declaration.return_type,
                    ColumnType::Integer | ColumnType::BigInteger
                )
        }) {
            output.push_produced(declaration.clone_with_control(control)?)?;
        }
        return Ok(output.finish()?);
    }
    let acl;
    let (types, result): (&[ColumnType], &ColumnType) = match name {
        "pg_listening_channels" => (&[], &ColumnType::Text),
        "unnest" => {
            let [Some(argument)] = arguments else {
                return Ok(output.finish()?);
            };
            let Some(element) = crate::type_resolution::array_element_type(argument) else {
                return Ok(output.finish()?);
            };
            (&[ColumnType::AnyArray], element)
        }
        "string_to_table" => (&[ColumnType::Text, ColumnType::Text], &ColumnType::Text),
        "json_array_elements" => (&[ColumnType::Json], &ColumnType::Json),
        "jsonb_array_elements" => (&[ColumnType::JsonB], &ColumnType::JsonB),
        "json_array_elements_text" | "json_object_keys" => (&[ColumnType::Json], &ColumnType::Text),
        "jsonb_array_elements_text" | "jsonb_object_keys" => {
            (&[ColumnType::JsonB], &ColumnType::Text)
        }
        "json_each" | "json_each_text" => (&[ColumnType::Json], &ColumnType::Record),
        "jsonb_each" | "jsonb_each_text" => (&[ColumnType::JsonB], &ColumnType::Record),
        "pg_get_sequence_data" => (&[ColumnType::Regclass], &ColumnType::Record),
        "pg_sequence_parameters" => (&[ColumnType::Oid], &ColumnType::Record),
        "aclexplode" => {
            acl = ColumnType::array_with_control(
                control.finish(ColumnType::AclItem, control.empty_reservation())?,
                control,
            )?;
            (std::slice::from_ref(&*acl), &ColumnType::Record)
        }
        _ => return Ok(output.finish()?),
    };
    let name = control.format(format_args!("pg_catalog.{name}"))?;
    let mut names = ProductionVec::new(*control);
    let mut argument_types = ProductionVec::new(*control);
    names.reserve(types.len())?;
    argument_types.reserve(types.len())?;
    for ty in types {
        names.push_produced(control.finish(None, control.empty_reservation())?)?;
        argument_types.push_produced(ty.clone_with_control(control)?)?;
    }
    let result = result.clone_with_control(control)?;
    let names = names.finish()?;
    let types = argument_types.finish()?;
    let (name, name_memory) = name.into_parts();
    let (argument_names, labels_memory) = names.into_parts();
    let (argument_types, types_memory) = types.into_parts();
    let (return_type, result_memory) = result.into_parts();
    output.push_produced(control.finish(
        BuiltinFunctionOverload {
            variadic: false,
            name,
            argument_names,
            argument_types,
            default_arguments: 0,
            return_type,
        },
        control.combine(
            name_memory,
            control.combine(labels_memory, control.combine(types_memory, result_memory)),
        ),
    )?)?;
    Ok(output.finish()?)
}
