//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table-result type inference retains its temporary and published descriptors under the invoking allowance.

use super::{
    ColumnType, FunctionParamMode, FunctionReturns, FunctionTypeResolver, RoutineInvocationBinding,
    RoutineResolution, SQLError, SQLParam, SQLUserFunction, TableFunctionAnalysis,
    TableFunctionTypeRequest,
};
use crate::{RowSchema, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

type Types<'a> = ProductionVec<'a, Option<ColumnType>>;

/// Type probes preserve their established unknown-type fallback; resource failures of a controlled producer remain failures.
pub(super) fn probe<T>(
    result: Result<T, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<Option<T>, SQLError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if control.budget().is_some()
                && matches!(error.sqlstate(), Some("53200" | "57014")) =>
        {
            Err(error)
        }
        Err(_) => Ok(None),
    }
}

fn push_type(
    output: &mut Types<'_>,
    value: Option<Produced<ColumnType>>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let (value, memory) = match value {
        Some(value) => {
            let (value, memory) = value.into_parts();
            (Some(value), memory)
        }
        None => (None, control.empty_reservation()),
    };
    output.push_produced(control.finish(value, memory)?)?;
    Ok(())
}

fn fixed<'a>(types: &[ColumnType], control: &ProductionControl<'a>) -> Result<Types<'a>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for ty in types {
        push_type(&mut output, Some(ty.clone_with_control(control)?), control)?;
    }
    Ok(output)
}

pub fn table_function_column_types_with_control(
    routines: &dyn RoutineResolution,
    request: TableFunctionTypeRequest<'_>,
    input: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    control.check()?;
    let width = request
        .columns
        .len()
        .saturating_sub(usize::from(request.ordinality));
    let ordinality = request.ordinality;
    let mut output = if !request.declared_types.is_empty() {
        let mut output = ProductionVec::new(*control);
        for name in request.declared_types {
            push_type(
                &mut output,
                probe(
                    ColumnType::from_sql_name_with_control(name, control),
                    control,
                )?,
                control,
            )?;
        }
        output
    } else if let Some(function) = request.user_function {
        user_function_types(routines, function, request.user_invocation, control)?
    } else {
        builtin_types(routines, request, input, params, resolver, control)?
    };
    if output.len() != width {
        let mut aligned = ProductionVec::new(*control);
        for _ in 0..width {
            let ty = match &*output {
                [Some(ty)] => Some(ty.clone_with_control(control)?),
                _ => None,
            };
            push_type(&mut aligned, ty, control)?;
        }
        output = aligned;
    }
    if ordinality {
        push_type(
            &mut output,
            Some(ColumnType::BigInteger.clone_with_control(control)?),
            control,
        )?;
    }
    Ok(output.finish()?)
}

fn builtin_types<'a>(
    routines: &dyn RoutineResolution,
    request: TableFunctionTypeRequest<'_>,
    input: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'a>,
) -> Result<Types<'a>, SQLError> {
    let name =
        crate::semantics::builtin_function_dispatch_name_with_control(request.name, control)?;
    match name.as_str() {
        "generate_series" => series_types(routines, request, input, params, resolver, control),
        "unnest" => {
            let mut output = ProductionVec::new(*control);
            for argument in request.args {
                push_type(
                    &mut output,
                    element_type(argument, input, params, resolver, control)?,
                    control,
                )?;
            }
            Ok(output)
        }
        "regexp_matches" => {
            let element = ColumnType::Text.clone_with_control(control)?;
            let ty = ColumnType::array_with_control(element, control)?;
            let mut output = ProductionVec::new(*control);
            push_type(&mut output, Some(ty), control)?;
            Ok(output)
        }
        "pg_listening_channels"
        | "regexp_split_to_table"
        | "string_to_table"
        | "json_object_keys"
        | "jsonb_object_keys"
        | "json_array_elements_text"
        | "jsonb_array_elements_text" => fixed(&[ColumnType::Text], control),
        "analyze_text" | "jsonb_array_elements" => fixed(&[ColumnType::JsonB], control),
        "json_array_elements" => fixed(&[ColumnType::Json], control),
        "json_each" => fixed(&[ColumnType::Text, ColumnType::Json], control),
        "jsonb_each" => fixed(&[ColumnType::Text, ColumnType::JsonB], control),
        "json_each_text" | "jsonb_each_text" => {
            fixed(&[ColumnType::Text, ColumnType::Text], control)
        }
        "pg_get_sequence_data" => fixed(&[ColumnType::BigInteger, ColumnType::Boolean], control),
        "pg_sequence_parameters" => fixed(
            &[
                ColumnType::BigInteger,
                ColumnType::BigInteger,
                ColumnType::BigInteger,
                ColumnType::BigInteger,
                ColumnType::Boolean,
                ColumnType::BigInteger,
                ColumnType::Oid,
            ],
            control,
        ),
        "aclexplode" => fixed(
            &[
                ColumnType::Oid,
                ColumnType::Oid,
                ColumnType::Text,
                ColumnType::Boolean,
            ],
            control,
        ),
        "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
        | "graph_betweenness" => fixed(
            &[ColumnType::BigInteger, ColumnType::DoublePrecision],
            control,
        ),
        "rpq" => fixed(&[ColumnType::BigInteger], control),
        "text_similarity_join"
        | "vector_similarity_join"
        | "graph_join"
        | "hybrid_join"
        | "cross_paradigm_join" => fixed(
            &[
                ColumnType::BigInteger,
                ColumnType::BigInteger,
                ColumnType::DoublePrecision,
            ],
            control,
        ),
        _ => user_call_types(
            routines,
            request.name,
            request.args,
            input,
            params,
            resolver,
            control,
        ),
    }
}

pub(super) fn element_type(
    argument: &ScalarExpr,
    input: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let ty = probe(
        crate::type_resolution::scalar_type_with_resolver_and_control(
            argument,
            input,
            params,
            Some(resolver),
            control,
        ),
        control,
    )?
    .flatten();
    ty.as_deref()
        .and_then(crate::type_resolution::array_element_type)
        .map(|ty| ty.clone_with_control(control).map_err(Into::into))
        .transpose()
}

fn user_call_types<'a>(
    routines: &dyn RoutineResolution,
    name: &str,
    args: &[ScalarExpr],
    input: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'a>,
) -> Result<Types<'a>, SQLError> {
    let Some(signature) = probe(
        crate::function_call_argument_signature_with_control(
            args,
            input,
            params,
            Some(resolver),
            control,
        ),
        control,
    )?
    else {
        return Ok(ProductionVec::new(*control));
    };
    let (names, types, variadic) = &*signature;
    let matched = probe(
        routines.resolve_static_sql_function_match_with_control(
            name,
            None,
            crate::type_resolution::RoutineCallDescriptor {
                argument_names: names,
                argument_types: types,
                explicit_variadic: *variadic,
            },
            control,
        ),
        control,
    )?
    .flatten();
    match matched {
        Some(matched) => user_function_types(
            routines,
            &matched.function,
            Some(&matched.invocation),
            control,
        ),
        None => Ok(ProductionVec::new(*control)),
    }
}

fn user_function_types<'a>(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
    control: &ProductionControl<'a>,
) -> Result<Types<'a>, SQLError> {
    let mut output = ProductionVec::new(*control);
    if let Some(descriptor) = probe(
        super::user_function_composite_result_with_control(routines, function, invocation, control),
        control,
    )?
    .flatten()
    {
        for attribute in &descriptor.attributes {
            push_type(
                &mut output,
                Some(attribute.ty.clone_with_control(control)?),
                control,
            )?;
        }
        return Ok(output);
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
        });
    if outputs.clone().next().is_some() {
        for (index, parameter) in outputs {
            let name = invocation
                .and_then(|binding| binding.parameter_types.get(index))
                .unwrap_or(&parameter.type_name);
            push_type(&mut output, resolve_type(routines, name, control)?, control)?;
        }
        return Ok(output);
    }
    if let FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } =
        &function.def.returns
    {
        let name = invocation
            .and_then(|binding| binding.return_type.as_ref())
            .unwrap_or(type_name);
        push_type(&mut output, resolve_type(routines, name, control)?, control)?;
    }
    Ok(output)
}

fn resolve_type(
    routines: &dyn RoutineResolution,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match probe(
        routines.resolve_type_name_with_control(name, control),
        control,
    )?
    .flatten()
    {
        Some(ty) => Ok(Some(ty)),
        None => probe(
            ColumnType::from_sql_name_with_control(name, control),
            control,
        ),
    }
}

fn series_types<'a>(
    routines: &dyn RoutineResolution,
    request: TableFunctionTypeRequest<'_>,
    input: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'a>,
) -> Result<Types<'a>, SQLError> {
    let binding = probe(
        super::resolve_table_function_binding_with_control(
            TableFunctionAnalysis {
                routines,
                types: resolver,
            },
            request.name,
            request.binding,
            request.args,
            input,
            params,
            control,
        ),
        control,
    )?
    .flatten();
    let ty = binding
        .as_ref()
        .map(|binding| {
            crate::type_resolution::fixed_builtin_return_type_with_control(binding, control)
        })
        .transpose()?
        .flatten();
    let mut output = ProductionVec::new(*control);
    push_type(&mut output, ty, control)?;
    Ok(output)
}
