//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column definition lists of functions in FROM, checked against the class of each function's result.

use crate::ast::{ColumnType, FunctionBinding, RoutineInvocationBinding};
use crate::routines::{RoutineResolution, SQLUserFunction};
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
pub(super) enum ResultClass {
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
    validate_table_function_column_definition_with_control(
        routines,
        request,
        input_schema,
        params,
        resolver,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub fn validate_table_function_column_definition_with_control(
    routines: &dyn RoutineResolution,
    request: ColumnDefinitionRequest<'_>,
    input_schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let class = match request.user_function {
        Some(function) => super::production::user_function_result_class_with_control(
            routines,
            function,
            request.user_invocation,
            control,
        )?,
        None if request.binding.is_some_and(|binding| !binding.builtin) => {
            ResultClass::Unclassified
        }
        None => builtin_result_class(&request, input_schema, params, resolver, control)?,
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
        ResultClass::Record => validate_record_column_definitions(routines, &request, control),
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
    routines: &dyn RoutineResolution,
    request: &ColumnDefinitionRequest<'_>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<(), SQLError> {
    if request.declared_types.len() > MAX_COLUMN_DEFINITIONS {
        return Err(SQLError::Routine {
            sqlstate: "54011".into(),
            message: format!(
                "column definition lists can have at most {MAX_COLUMN_DEFINITIONS} entries"
            ),
        });
    }
    let mut pseudo_types = uqa_core::memory::ProductionVec::new(*control);
    for (position, declared) in request.declared_types.iter().enumerate() {
        let column = request
            .column_names
            .get(position)
            .map_or("", String::as_str);
        if declared.starts_with(SETOF_COLUMN_DEFINITION_PREFIX) {
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!("column \"{column}\" cannot be declared SETOF"),
            });
        }
        let pseudo = crate::catalog::type_metadata::routine_pseudo_type_name(
            crate::catalog::type_metadata::routine_type_oid(declared),
        );
        if pseudo.is_none() {
            let ty = routines
                .resolve_type_name_with_control(declared, control)?
                .map_or_else(
                    || {
                        ColumnType::from_sql_name_with_control(declared, control).map_err(|error| {
                            if matches!(error.sqlstate(), Some("53200" | "57014")) {
                                error
                            } else {
                                SQLError::Routine {
                                    sqlstate: "42704".into(),
                                    message: format!("type \"{declared}\" does not exist"),
                                }
                            }
                        })
                    },
                    Ok,
                )?;
            routines.require_type_usage(&ty)?;
        }
        pseudo_types.push_copy(pseudo)?;
    }
    let mut names = uqa_core::memory::OwnedSet::new();
    for name in request.column_names {
        control.check()?;
        let inserted = match control.budget() {
            Some(budget) => names.insert_budgeted(name, budget)?,
            None => names.insert(name),
        };
        if !inserted {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column name \"{name}\" specified more than once"),
            });
        }
    }
    for (column, pseudo) in request
        .column_names
        .iter()
        .zip(pseudo_types.iter().copied())
    {
        // Function result descriptions may contain anonymous records, unlike stored table rows.
        if let Some(pseudo) = pseudo.filter(|pseudo| *pseudo != "record") {
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!("column \"{column}\" has pseudo-type {pseudo}"),
            });
        }
    }
    Ok(())
}

fn builtin_result_class(
    request: &ColumnDefinitionRequest<'_>,
    input_schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<ResultClass, SQLError> {
    let builtin =
        crate::semantics::builtin_function_dispatch_name_with_control(request.name, control)?;
    Ok(match builtin.as_str() {
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
                super::types::element_type(argument, input_schema, params, resolver, control)?
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
    })
}

/// The class of a result of type `ty`; a domain takes its base type's class, as a domain over a composite type does.
pub(super) fn type_result_class(ty: &ColumnType) -> ResultClass {
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
