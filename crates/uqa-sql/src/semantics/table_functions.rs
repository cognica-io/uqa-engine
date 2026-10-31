//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table-function binding, declared result types, and source aliases.

use crate::ast::{
    ColumnType, FunctionBinding, FunctionParamMode, FunctionReturns, RoutineInvocationBinding,
};
use crate::routines::{RoutineResolution, SQLUserFunction};
use crate::type_resolution::{BuiltinFunctionOverload, FunctionTypeResolver};
use crate::{RowSchema, SQLError, SQLParam, ScalarExpr};
use std::sync::Arc;

mod columns;
pub use columns::table_function_empty_schema_with_control;
mod production;
mod selection;
mod types;
pub use production::{
    apply_table_function_aliases_with_control, user_function_composite_result_with_control,
    user_function_declared_columns_with_control, user_function_output_columns_with_control,
    user_table_function_columns_with_control, validate_table_function_alias_count_with_control,
};
pub(crate) use selection::resolve_table_function_binding_for_types_with_control;
pub use types::table_function_column_types_with_control;

mod column_definitions;
pub use column_definitions::{
    validate_table_function_column_definition,
    validate_table_function_column_definition_with_control, ColumnDefinitionRequest,
    SETOF_COLUMN_DEFINITION_PREFIX,
};

pub struct ResolvedUserTableFunction {
    pub function: Arc<SQLUserFunction>,
    pub binding: FunctionBinding,
}

/// Correct the old integer series declaration, which recorded a nonexistent default on the three-argument overload. This only accepts the exact builtin catalog shape; it never resolves a persisted call by its display name.
pub(crate) fn upgrade_legacy_table_function_binding(
    binding: &mut Option<FunctionBinding>,
    argument_count: usize,
) -> bool {
    let Some(binding) = binding else {
        return false;
    };
    if argument_count != 2
        || !binding.builtin
        || binding.object_id.is_some()
        || binding.name != "pg_catalog.generate_series"
        || binding.dispatch.is_some()
        || binding.invocation.is_some()
        || binding.resolution_error.is_some()
    {
        return false;
    }
    let [first, second, third] = binding.argument_types.as_slice() else {
        return false;
    };
    if !matches!(first.as_str(), "integer" | "bigint") || first != second || first != third {
        return false;
    }
    binding.argument_types.truncate(2);
    true
}

pub fn user_function_output_columns_for(function: &SQLUserFunction) -> Option<Vec<String>> {
    user_function_output_columns_with_control(
        function,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary function output columns")
    .map(|columns| columns.into_uncontrolled().expect("ordinary output names"))
}

/// A named composite returned by a function expands to its live attributes in FROM, including a single composite OUT parameter.
pub fn user_function_composite_result(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
) -> Result<Option<Arc<crate::expr::composites::CompositeTypeDescriptor>>, SQLError> {
    production::composite_result_oid_with_control(
        routines,
        function,
        invocation,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .map(|oid| crate::expr::composites::descriptor(routines.composite_types(), oid))
    .transpose()
}

/// The columns a user function in FROM declares, before column aliases: a named composite result's attributes, including those of a single composite OUT parameter, or else the OUT parameters. `None` for a function with neither, whose single column the FROM item names.
pub fn user_function_declared_columns(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
) -> Result<Option<Vec<String>>, SQLError> {
    user_function_declared_columns_with_control(
        routines,
        function,
        invocation,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|columns| {
        columns.map(|columns| {
            columns
                .into_uncontrolled()
                .expect("ordinary declared column names")
        })
    })
}

/// Derive a user FROM function's width from its result type before applying aliases. Only an anonymous record takes its shape from the column definition list; aliases never add fields to a scalar result.
pub fn user_table_function_columns(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
    output_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
    ordinality: bool,
) -> Result<Vec<String>, SQLError> {
    user_table_function_columns_with_control(
        routines,
        function,
        invocation,
        output_name,
        alias,
        column_aliases,
        ordinality,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|columns| {
        columns
            .into_uncontrolled()
            .expect("ordinary table function names")
    })
}

pub fn resolve_user_table_function(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    input_schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<ResolvedUserTableFunction>, SQLError> {
    resolve_user_table_function_with_control(
        TableFunctionAnalysis {
            routines,
            types: resolver,
        },
        name,
        binding,
        args,
        input_schema,
        params,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .map(|resolved| {
        Ok(ResolvedUserTableFunction {
            function: resolved.function,
            binding: resolved
                .binding
                .into_uncontrolled()
                .expect("ordinary table binding"),
        })
    })
    .transpose()
}

pub struct ControlledUserTableFunction {
    pub function: Arc<SQLUserFunction>,
    pub binding: uqa_core::memory::Produced<FunctionBinding>,
}

pub fn resolve_user_table_function_with_control(
    context: TableFunctionAnalysis<'_>,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    input: &RowSchema,
    params: &[SQLParam],
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<ControlledUserTableFunction>, SQLError> {
    let Some(binding) = resolve_table_function_binding_with_control(
        TableFunctionAnalysis { ..context },
        name,
        binding,
        args,
        input,
        params,
        control,
    )?
    else {
        return Ok(None);
    };
    if binding.builtin {
        return Ok(None);
    }
    let signature = crate::function_call_argument_signature_with_control(
        args,
        input,
        params,
        Some(context.types),
        control,
    )?;
    let (argument_names, argument_types, explicit_variadic) = &*signature;
    let Some(matched) = context
        .routines
        .resolve_static_sql_function_match_with_control(
            name,
            Some(&binding),
            crate::type_resolution::RoutineCallDescriptor {
                argument_names,
                argument_types,
                explicit_variadic: *explicit_variadic,
            },
            control,
        )?
    else {
        return Ok(None);
    };
    let binding = crate::routines::resolution::selected_binding(
        &matched.function,
        matched.invocation,
        control,
    )?;
    Ok(Some(ControlledUserTableFunction {
        function: matched.function,
        binding,
    }))
}

pub struct TableFunctionAnalysis<'a> {
    pub routines: &'a dyn RoutineResolution,
    pub types: &'a dyn FunctionTypeResolver,
}

pub fn resolve_table_function_binding(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    input_schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<FunctionBinding>, SQLError> {
    Ok(resolve_table_function_binding_with_control(
        TableFunctionAnalysis {
            routines,
            types: resolver,
        },
        name,
        binding,
        args,
        input_schema,
        params,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .map(|binding| binding.into_uncontrolled().expect("ordinary table binding")))
}

pub fn resolve_table_function_binding_with_control(
    context: TableFunctionAnalysis<'_>,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    input_schema: &RowSchema,
    params: &[SQLParam],
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<FunctionBinding>>, SQLError> {
    control.check()?;
    if let Some(binding) = binding {
        return Ok(Some(binding.clone_with_control(control)?));
    }
    let signature = crate::function_call_argument_signature_with_control(
        args,
        input_schema,
        params,
        Some(context.types),
        control,
    )?;
    let (argument_names, argument_types, explicit_variadic) = &*signature;
    if crate::type_resolution::function_arguments_have_runtime_types_with_control(
        args,
        argument_types,
        control,
    )? {
        return Ok(None);
    }
    resolve_table_function_binding_for_types_with_control(
        context.routines,
        name,
        binding,
        crate::type_resolution::RoutineCallDescriptor {
            argument_names,
            argument_types,
            explicit_variadic: *explicit_variadic,
        },
        control,
    )
}

/// Select a call whose dynamic inputs have now produced values, preserving every statically declared argument type and the original SQL unknown-literal rules.
pub fn resolve_runtime_table_function_binding(
    routines: &dyn RoutineResolution,
    name: &str,
    args: &[ScalarExpr],
    values: &[(Option<String>, uqa_core::Value)],
    input_schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<FunctionBinding>, SQLError> {
    let (argument_names, mut argument_types, explicit_variadic) =
        crate::function_call_argument_signature(args, input_schema, params, Some(resolver))?;
    let arguments = crate::scalar_call_arguments(args)?;
    assert_eq!(arguments.len(), values.len());
    for ((argument, ty), (_, value)) in arguments.iter().zip(&mut argument_types).zip(values) {
        if crate::type_resolution::runtime_input(argument.value, ty.as_ref()) {
            *ty = crate::type_resolution::value_type(value);
        }
    }
    resolve_table_function_binding_for_types(
        routines,
        name,
        None,
        crate::type_resolution::RoutineCallDescriptor {
            argument_names: &argument_names,
            argument_types: &argument_types,
            explicit_variadic,
        },
    )
}

/// Select a FROM call from arguments already analyzed in their owning scalar-subquery scope.
pub(crate) fn resolve_table_function_binding_for_types(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    call: crate::type_resolution::RoutineCallDescriptor<'_>,
) -> Result<Option<FunctionBinding>, SQLError> {
    Ok(resolve_table_function_binding_for_types_with_control(
        routines,
        name,
        binding,
        call,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .map(|binding| binding.into_uncontrolled().expect("ordinary table binding")))
}

fn has_builtin_table_function_overloads(name: &str) -> bool {
    matches!(
        name,
        "generate_series"
            | "pg_listening_channels"
            | "unnest"
            | "regexp_matches"
            | "regexp_split_to_table"
            | "string_to_table"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_object_keys"
            | "jsonb_object_keys"
            | "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
            | "pg_get_sequence_data"
            | "pg_sequence_parameters"
            | "aclexplode"
    )
}

#[cfg(test)]
fn builtin_table_function_overloads(
    name: &str,
    argument_types: &[Option<ColumnType>],
) -> Vec<BuiltinFunctionOverload> {
    selection::builtin_table_function_overloads_with_control(
        name,
        argument_types,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary table declarations cannot be cancelled or limited")
    .into_uncontrolled()
    .expect("ordinary table declarations")
}

pub fn is_builtin_table_function(name: &str) -> bool {
    matches!(
        name,
        "generate_series"
            | "pg_listening_channels"
            | "unnest"
            | "regexp_matches"
            | "regexp_split_to_table"
            | "string_to_table"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_object_keys"
            | "jsonb_object_keys"
            | "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
            | "pg_get_sequence_data"
            | "pg_sequence_parameters"
            | "aclexplode"
            | "create_analyzer"
            | "drop_analyzer"
            | "list_analyzers"
            | "analyze_text"
            | "fts_index_stats"
            | "set_table_analyzer"
            | "pagerank"
            | "graph_pagerank"
            | "hits"
            | "graph_hits"
            | "betweenness"
            | "graph_betweenness"
            | "graph_edges"
            | "rpq"
            | "cypher"
    )
}

pub fn table_function_empty_schema(
    name: &str,
    output_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
    output_width: usize,
    ordinality: bool,
) -> Vec<String> {
    table_function_empty_schema_with_control(
        name,
        output_name,
        alias,
        column_aliases,
        output_width,
        ordinality,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary table-function names")
    .into_uncontrolled()
    .expect("ordinary table-function owner")
}

pub fn apply_table_function_aliases(
    mut columns: Vec<String>,
    column_aliases: &[String],
    ordinality: bool,
) -> Vec<String> {
    if ordinality {
        columns.push("ordinality".into());
    }
    for (column, alias) in columns.iter_mut().zip(column_aliases) {
        column.clone_from(alias);
    }
    columns
}

pub fn validate_table_function_alias_count(
    table_alias: &str,
    available: usize,
    specified: usize,
) -> Result<(), SQLError> {
    validate_table_function_alias_count_with_control(
        table_alias,
        available,
        specified,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub struct TableFunctionTypeRequest<'a> {
    pub name: &'a str,
    pub binding: Option<&'a FunctionBinding>,
    pub args: &'a [ScalarExpr],
    pub user_function: Option<&'a SQLUserFunction>,
    pub user_invocation: Option<&'a RoutineInvocationBinding>,
    pub declared_types: &'a [String],
    pub columns: &'a [String],
    pub ordinality: bool,
}

pub fn table_function_column_types(
    routines: &dyn RoutineResolution,
    request: TableFunctionTypeRequest<'_>,
    input_schema: &crate::RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Vec<Option<ColumnType>> {
    table_function_column_types_with_control(
        routines,
        request,
        input_schema,
        params,
        resolver,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary table result types")
    .into_uncontrolled()
    .expect("ordinary table type fields")
}

pub fn is_json_array_table_function(name: &str) -> bool {
    matches!(
        name,
        "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
    )
}

pub fn scalar_table_function_default_column(
    normalized_name: &str,
    output_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
) -> String {
    column_aliases.first().cloned().unwrap_or_else(|| {
        if is_json_array_table_function(normalized_name) {
            "value".into()
        } else {
            alias.unwrap_or(output_name).to_string()
        }
    })
}
pub fn join_alias_columns(
    schema: &crate::RowSchema,
    alias: &str,
    column_aliases: &[String],
) -> Result<Vec<String>, SQLError> {
    Ok(join_alias_columns_with_control(
        schema,
        alias,
        column_aliases,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary JOIN alias has no reservation"))
}

pub fn alias_join_schema(
    schema: &crate::RowSchema,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<crate::RowSchema, SQLError> {
    alias_join_schema_with_control(
        schema,
        alias,
        column_aliases,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

mod join_aliases;
pub use join_aliases::{
    alias_join_physical_schema_with_control, alias_join_schema_with_control,
    join_alias_columns_with_control,
};

#[cfg(test)]
mod tests;
