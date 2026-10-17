//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column definition lists of functions in FROM, checked against the class of each function's result.

use crate::ast::{
    ColumnType, FunctionBinding, FunctionParamMode, FunctionReturns, RoutineInvocationBinding,
};
use crate::routines::{routine_returns_anonymous_record, RoutineResolution, SQLUserFunction};
use crate::type_resolution::FunctionTypeResolver;
use crate::{RowSchema, SQLError, SQLParam, ScalarExpr};

/// The marker a column definition list keeps for a column declared SETOF.
pub const SETOF_COLUMN_DEFINITION_PREFIX: &str = "setof ";

/// `PostgreSQL` limits a column definition list to `MaxHeapAttributeNumber` columns.
const MAX_COLUMN_DEFINITIONS: usize = 1600;

/// A function in FROM with the column definition list written for it.
pub struct ColumnDefinitionRequest<'a> {
    pub name: &'a str,
    pub binding: Option<&'a FunctionBinding>,
    pub user_function: Option<&'a SQLUserFunction>,
    /// The concrete types the call selected for a user function, which give a polymorphic result its actual type.
    pub user_invocation: Option<&'a RoutineInvocationBinding>,
    pub args: &'a [ScalarExpr],
    pub declared_types: &'a [String],
    pub column_names: &'a [String],
}

/// The class of a FROM function's result, which decides whether a column definition list describes it, as `get_expr_result_type` classifies it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultClass {
    /// An anonymous record, which only a column definition list describes.
    Record,
    /// A record that OUT parameters describe.
    OutParameters,
    NamedComposite,
    Scalar,
    /// A UQA extension or host-registered table function, whose result the catalog does not classify.
    Unclassified,
}

/// Check the column definition list of a function in FROM against the class of its result, as `addRangeTableEntryForFunction` does.
pub fn validate_table_function_column_definition(
    routines: &dyn RoutineResolution,
    request: ColumnDefinitionRequest<'_>,
    input_schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    let class = match request.user_function {
        Some(function) => user_function_result_class(routines, function, request.user_invocation)?,
        None if request.binding.is_some_and(|binding| !binding.builtin) => {
            ResultClass::Unclassified
        }
        None => builtin_result_class(&request, input_schema, params, resolver),
    };
    let syntax_error = |message: &str| SQLError::Routine {
        sqlstate: "42601".into(),
        message: message.into(),
    };
    if request.declared_types.is_empty() {
        return if class == ResultClass::Record {
            Err(syntax_error(
                "a column definition list is required for functions returning \"record\"",
            ))
        } else {
            Ok(())
        };
    }
    match class {
        ResultClass::Record => validate_record_column_definitions(&request),
        ResultClass::OutParameters => Err(redundant_out_column_definition_error()),
        ResultClass::NamedComposite => Err(syntax_error(
            "a column definition list is redundant for a function returning a named composite type",
        )),
        ResultClass::Scalar => Err(syntax_error(
            "a column definition list is only allowed for functions returning \"record\"",
        )),
        ResultClass::Unclassified => Ok(()),
    }
}

/// A column definition list describes an anonymous record with at most `MaxHeapAttributeNumber` ordinary columns.
fn validate_record_column_definitions(
    request: &ColumnDefinitionRequest<'_>,
) -> Result<(), SQLError> {
    if request.declared_types.len() > MAX_COLUMN_DEFINITIONS {
        return Err(SQLError::Routine {
            sqlstate: "54011".into(),
            message: format!(
                "column definition lists can have at most {MAX_COLUMN_DEFINITIONS} entries"
            ),
        });
    }
    for (position, declared) in request.declared_types.iter().enumerate() {
        if declared.starts_with(SETOF_COLUMN_DEFINITION_PREFIX) {
            let column = request
                .column_names
                .get(position)
                .map_or("", String::as_str);
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!("column \"{column}\" cannot be declared SETOF"),
            });
        }
    }
    Ok(())
}

/// The class of a user function's result for one call; a polymorphic result takes the type the call resolved for it, as `get_expr_result_type` resolves it from the actual arguments.
fn user_function_result_class(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
) -> Result<ResultClass, SQLError> {
    if routine_returns_anonymous_record(&function.def) {
        return Ok(ResultClass::Record);
    }
    if function.def.output_params().len() > 1 {
        return Ok(ResultClass::OutParameters);
    }
    // A single OUT parameter, such as the one column of RETURNS TABLE, gives the result its type.
    let output = function
        .def
        .params
        .iter()
        .enumerate()
        .find(|(_, parameter)| {
            matches!(
                parameter.mode,
                FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
            )
        });
    let (declared, concrete) = match (output, &function.def.returns) {
        (Some((position, output)), _) => (
            output.type_name.as_str(),
            invocation
                .and_then(|invocation| invocation.parameter_types.get(position))
                .map(String::as_str),
        ),
        (None, FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name }) => (
            type_name.as_str(),
            invocation.and_then(|invocation| invocation.return_type.as_deref()),
        ),
        _ => return Ok(ResultClass::Scalar),
    };
    let type_name = concrete.unwrap_or(declared);
    if crate::type_resolution::routine_polymorphic_type(type_name).is_some() {
        return Ok(ResultClass::Unclassified);
    }
    Ok(match routines.resolve_type_name(type_name)? {
        Some(ty) => type_result_class(&ty),
        None => ResultClass::Scalar,
    })
}

fn builtin_result_class(
    request: &ColumnDefinitionRequest<'_>,
    input_schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> ResultClass {
    let builtin =
        crate::semantics::builtin_function_dispatch_name(&request.name.to_ascii_lowercase());
    match builtin.as_str() {
        "json_each"
        | "jsonb_each"
        | "json_each_text"
        | "jsonb_each_text"
        | "pg_get_sequence_data"
        | "pg_sequence_parameters"
        | "aclexplode" => ResultClass::OutParameters,
        "generate_series"
        | "pg_listening_channels"
        | "regexp_matches"
        | "regexp_split_to_table"
        | "string_to_table"
        | "json_array_elements"
        | "jsonb_array_elements"
        | "json_array_elements_text"
        | "jsonb_array_elements_text"
        | "json_object_keys"
        | "jsonb_object_keys" => ResultClass::Scalar,
        // `unnest` returns its array's element type; an array of anonymous records produces one record column, which this check leaves unclassified.
        "unnest" => match request.args {
            [argument] => {
                crate::scalar_type_with_resolver(argument, input_schema, params, resolver)
                    .ok()
                    .flatten()
                    .and_then(|ty| crate::type_resolution::array_element_type(&ty).cloned())
                    .map_or(
                        ResultClass::Unclassified,
                        |element| match type_result_class(&element) {
                            ResultClass::Record => ResultClass::Unclassified,
                            class => class,
                        },
                    )
            }
            _ => ResultClass::Unclassified,
        },
        _ => ResultClass::Unclassified,
    }
}

/// The class of a result of type `ty`; a domain takes its base type's class, as a domain over a composite type does.
fn type_result_class(ty: &ColumnType) -> ResultClass {
    match ty {
        ColumnType::Domain { base, .. } => type_result_class(base),
        ColumnType::Composite(_) => ResultClass::NamedComposite,
        ColumnType::Record => ResultClass::Record,
        _ => ResultClass::Scalar,
    }
}

fn redundant_out_column_definition_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: "a column definition list is redundant for a function with OUT parameters".into(),
    }
}
