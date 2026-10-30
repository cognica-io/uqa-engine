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
pub use production::{
    apply_table_function_aliases_with_control, user_function_composite_result_with_control,
    user_function_declared_columns_with_control, user_function_output_columns_with_control,
    user_table_function_columns_with_control, validate_table_function_alias_count_with_control,
};
pub(crate) use selection::resolve_table_function_binding_for_types_with_control;

mod column_definitions;
pub use column_definitions::{
    validate_table_function_column_definition, ColumnDefinitionRequest,
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
    let Some(binding) = resolve_table_function_binding(
        routines,
        name,
        binding,
        args,
        input_schema,
        params,
        resolver,
    )?
    else {
        return Ok(None);
    };
    if binding.builtin {
        return Ok(None);
    }
    let (argument_names, argument_types, explicit_variadic) =
        crate::function_call_argument_signature(args, input_schema, params, Some(resolver))?;
    let Some(matched) = routines.resolve_static_sql_function_match(
        name,
        Some(&binding),
        &argument_names,
        &argument_types,
        explicit_variadic,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(ResolvedUserTableFunction {
        binding: matched.binding(),
        function: matched.function,
    }))
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
    if let Some(binding) = binding {
        return Ok(Some(binding.clone()));
    }
    let (argument_names, argument_types, explicit_variadic) =
        crate::function_call_argument_signature(args, input_schema, params, Some(resolver))?;
    if crate::type_resolution::function_arguments_have_runtime_types(args, &argument_types)? {
        return Ok(None);
    }
    resolve_table_function_binding_for_types(
        routines,
        name,
        binding,
        crate::type_resolution::RoutineCallDescriptor {
            argument_names: &argument_names,
            argument_types: &argument_types,
            explicit_variadic,
        },
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

#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub fn table_function_column_types(
    routines: &dyn RoutineResolution,
    request: TableFunctionTypeRequest<'_>,
    input_schema: &crate::RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Vec<Option<ColumnType>> {
    let TableFunctionTypeRequest {
        name,
        binding,
        args,
        user_function,
        user_invocation,
        declared_types,
        columns,
        ordinality,
    } = request;
    let value_columns = if ordinality {
        columns
            .get(..columns.len().saturating_sub(1))
            .unwrap_or(&[])
    } else {
        columns
    };
    let align = |types: Vec<Option<ColumnType>>| {
        if types.len() == value_columns.len() {
            types
        } else if let [ty] = types.as_slice() {
            vec![ty.clone(); value_columns.len()]
        } else {
            vec![None; value_columns.len()]
        }
    };
    let mut types = if !declared_types.is_empty() {
        align(
            declared_types
                .iter()
                .map(|ty| ColumnType::from_sql_name(ty).ok())
                .collect(),
        )
    } else if let Some(function) = user_function {
        align(user_function_column_types(
            routines,
            function,
            user_invocation,
        ))
    } else {
        let normalized = crate::semantics::builtin_function_dispatch_name(name);
        align(match normalized.as_ref() {
            "pg_listening_channels" => vec![Some(ColumnType::Text)],
            "generate_series" => vec![resolve_table_function_binding(
                routines,
                name,
                binding,
                args,
                input_schema,
                params,
                resolver,
            )
            .ok()
            .flatten()
            .as_ref()
            .and_then(crate::type_resolution::fixed_builtin_return_type)],
            "analyze_text" => vec![Some(ColumnType::JsonB)],
            "unnest" => args
                .iter()
                .map(|argument| {
                    crate::scalar_type_with_resolver(argument, input_schema, params, resolver)
                        .ok()
                        .flatten()
                        .and_then(|ty| crate::type_resolution::array_element_type(&ty).cloned())
                })
                .collect(),
            "regexp_matches" => vec![Some(ColumnType::Array(Box::new(ColumnType::Text)))],
            "regexp_split_to_table"
            | "string_to_table"
            | "json_object_keys"
            | "jsonb_object_keys" => vec![Some(ColumnType::Text)],
            "json_array_elements" => vec![Some(ColumnType::Json)],
            "jsonb_array_elements" => vec![Some(ColumnType::JsonB)],
            "json_array_elements_text" | "jsonb_array_elements_text" => {
                vec![Some(ColumnType::Text)]
            }
            "json_each" => vec![Some(ColumnType::Text), Some(ColumnType::Json)],
            "jsonb_each" => vec![Some(ColumnType::Text), Some(ColumnType::JsonB)],
            "json_each_text" | "jsonb_each_text" => {
                vec![Some(ColumnType::Text), Some(ColumnType::Text)]
            }
            "pg_get_sequence_data" => {
                vec![Some(ColumnType::BigInteger), Some(ColumnType::Boolean)]
            }
            "pg_sequence_parameters" => vec![
                Some(ColumnType::BigInteger),
                Some(ColumnType::BigInteger),
                Some(ColumnType::BigInteger),
                Some(ColumnType::BigInteger),
                Some(ColumnType::Boolean),
                Some(ColumnType::BigInteger),
                Some(ColumnType::Oid),
            ],
            "aclexplode" => vec![
                Some(ColumnType::Oid),
                Some(ColumnType::Oid),
                Some(ColumnType::Text),
                Some(ColumnType::Boolean),
            ],
            "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
            | "graph_betweenness" => vec![
                Some(ColumnType::BigInteger),
                Some(ColumnType::DoublePrecision),
            ],
            "rpq" => vec![Some(ColumnType::BigInteger)],
            "text_similarity_join"
            | "vector_similarity_join"
            | "graph_join"
            | "hybrid_join"
            | "cross_paradigm_join" => vec![
                Some(ColumnType::BigInteger),
                Some(ColumnType::BigInteger),
                Some(ColumnType::DoublePrecision),
            ],
            _ => user_table_function_column_types(
                routines,
                name,
                args,
                input_schema,
                params,
                resolver,
            ),
        })
    };
    if ordinality {
        types.push(Some(ColumnType::BigInteger));
    }
    types
}

fn user_table_function_column_types(
    routines: &dyn RoutineResolution,
    name: &str,
    args: &[ScalarExpr],
    input_schema: &crate::RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Vec<Option<ColumnType>> {
    let Ok((argument_names, argument_types, explicit_variadic)) =
        crate::function_call_argument_signature(args, input_schema, params, Some(resolver))
    else {
        return Vec::new();
    };
    let Ok(Some(matched)) = routines.resolve_static_sql_function_match(
        name,
        None,
        &argument_names,
        &argument_types,
        explicit_variadic,
    ) else {
        return Vec::new();
    };
    user_function_column_types(routines, &matched.function, Some(&matched.invocation))
}

fn user_function_column_types(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
) -> Vec<Option<ColumnType>> {
    if let Ok(Some(descriptor)) = user_function_composite_result(routines, function, invocation) {
        return descriptor
            .attributes
            .iter()
            .map(|attribute| Some(attribute.ty.clone()))
            .collect();
    }
    let outputs = function
        .def
        .params
        .iter()
        .enumerate()
        .filter(|(_, parameter)| {
            matches!(
                parameter.mode,
                FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
            )
        })
        .collect::<Vec<_>>();
    if !outputs.is_empty() {
        return outputs
            .into_iter()
            .map(|(index, parameter)| {
                let type_name = invocation
                    .and_then(|binding| binding.parameter_types.get(index))
                    .unwrap_or(&parameter.type_name);
                resolve_table_function_column_type(routines, type_name)
            })
            .collect();
    }
    match &function.def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            let type_name = invocation
                .and_then(|binding| binding.return_type.as_ref())
                .unwrap_or(type_name);
            vec![resolve_table_function_column_type(routines, type_name)]
        }
        FunctionReturns::None | FunctionReturns::Table => Vec::new(),
    }
}

fn resolve_table_function_column_type(
    routines: &dyn RoutineResolution,
    type_name: &str,
) -> Option<ColumnType> {
    routines
        .resolve_type_name(type_name)
        .ok()
        .flatten()
        .or_else(|| ColumnType::from_sql_name(type_name).ok())
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
    let available = schema.len();
    let specified = column_aliases.len();
    if specified > available {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message: format!(
                "join expression \"{alias}\" has {available} columns available but {specified} columns specified"
            ),
        });
    }
    Ok(schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| {
            column_aliases
                .get(position)
                .cloned()
                .unwrap_or_else(|| schema.public_name(position).unwrap_or(column).to_string())
        })
        .collect())
}

pub fn alias_join_schema(
    schema: &crate::RowSchema,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<crate::RowSchema, SQLError> {
    let Some(alias) = alias else {
        if column_aliases.is_empty() {
            return Ok(schema.clone());
        }
        return Err(SQLError::Internal(
            "JOIN column aliases exist without a relation alias".into(),
        ));
    };
    let columns = join_alias_columns(schema, alias, column_aliases)?;
    Ok(
        crate::RowSchema::with_qualified_types(alias, columns, schema.column_types().to_vec())
            .with_column_diagnostic_names_from(schema),
    )
}

#[cfg(test)]
mod tests;
