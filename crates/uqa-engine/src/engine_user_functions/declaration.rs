//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine type resolution, declaration validation, and body compilation.

use uqa_execution::ScalarExpr;
use uqa_planner::UnifiedPlan;
use uqa_sql::ast::{
    AlterRoutineStmt, ColumnType, CreateFunction, FunctionBody, FunctionParamMode, FunctionReturns,
    RoutineColumnTypeReference, Statement,
};
use uqa_sql::SQLError;

use crate::Engine;

use super::{canonical_routine_type_name, routine_local_name, CompiledFunctionBody};

pub(super) fn resolve_routine_type_references(
    engine: &Engine,
    def: &mut CreateFunction,
) -> Result<(), SQLError> {
    for parameter in &mut def.params {
        parameter.type_name = resolve_routine_type_name_with_reference(
            engine,
            &parameter.type_name,
            ROUTINE_PARAMETER_PSEUDO_TYPES,
            parameter.type_reference.as_ref(),
        )?;
        parameter.type_reference = None;
    }
    match &mut def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            *type_name = resolve_routine_type_name_with_reference(
                engine,
                type_name,
                ROUTINE_RESULT_PSEUDO_TYPES,
                def.return_type_reference.as_ref(),
            )?;
        }
        FunctionReturns::None | FunctionReturns::Table => {}
    }
    def.return_type_reference = None;
    Ok(())
}

pub(super) fn resolve_alter_routine_identity_types(
    engine: &Engine,
    stmt: &AlterRoutineStmt,
) -> Result<Option<Vec<String>>, SQLError> {
    let Some(types) = stmt.arg_types.as_ref() else {
        if !stmt.arg_type_references.is_empty() {
            return Err(SQLError::Internal(
                "ALTER routine omitted its identity types but retained type references".into(),
            ));
        }
        return Ok(None);
    };
    if !stmt.arg_type_references.is_empty() && stmt.arg_type_references.len() != types.len() {
        return Err(SQLError::Internal(format!(
            "ALTER routine has {} identity types but {} type references",
            types.len(),
            stmt.arg_type_references.len()
        )));
    }
    types
        .iter()
        .enumerate()
        .map(|(index, type_name)| {
            resolve_routine_type_name_with_reference(
                engine,
                type_name,
                ROUTINE_PARAMETER_PSEUDO_TYPES,
                stmt.arg_type_references.get(index).and_then(Option::as_ref),
            )
            .map(|resolved| canonical_routine_type_name(&resolved))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

const POLYMORPHIC_PSEUDO_TYPES: &[&str] = &[
    "anyelement",
    "anyarray",
    "anynonarray",
    "anyenum",
    "anyrange",
    "anymultirange",
    "anycompatible",
    "anycompatiblearray",
    "anycompatiblenonarray",
    "anycompatiblerange",
    "anycompatiblemultirange",
];

const ROUTINE_PARAMETER_PSEUDO_TYPES: &[&str] = &[
    "record",
    "refcursor",
    "cstring",
    "any",
    "void",
    "trigger",
    "internal",
    "event_trigger",
    "anyelement",
    "anyarray",
    "anynonarray",
    "anyenum",
    "anyrange",
    "anymultirange",
    "anycompatible",
    "anycompatiblearray",
    "anycompatiblenonarray",
    "anycompatiblerange",
    "anycompatiblemultirange",
];

const ROUTINE_RESULT_PSEUDO_TYPES: &[&str] = &[
    "record",
    "refcursor",
    "cstring",
    "any",
    "void",
    "trigger",
    "internal",
    "event_trigger",
    "anyelement",
    "anyarray",
    "anynonarray",
    "anyenum",
    "anyrange",
    "anymultirange",
    "anycompatible",
    "anycompatiblearray",
    "anycompatiblenonarray",
    "anycompatiblerange",
    "anycompatiblemultirange",
];

fn resolve_routine_type_name_with_reference(
    engine: &Engine,
    type_name: &str,
    allowed_pseudo_types: &[&str],
    structured_reference: Option<&RoutineColumnTypeReference>,
) -> Result<String, SQLError> {
    let mut base = type_name.trim();
    let mut array_dimensions = 0usize;
    while let Some(element) = base.strip_suffix("[]") {
        base = element.trim_end();
        array_dimensions += 1;
    }
    let resolved = if base
        .get(base.len().saturating_sub("%type".len())..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case("%type"))
    {
        let reference = structured_reference.ok_or_else(|| {
            SQLError::Internal(format!(
                "routine type reference `{type_name}` is missing structured relation-column identity"
            ))
        })?;
        let table = reference.relation_reference();
        let columns = engine
            .try_describe_table(&table)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "resolve routine type reference `{type_name}`: {error}"
                ))
            })?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
        columns
            .into_iter()
            .find(|definition| definition.name == reference.column)
            .map(|definition| definition.ty)
            .ok_or_else(|| SQLError::UnknownColumn(reference.type_reference()))?
    } else {
        let canonical = canonical_routine_type_name(base);
        if allowed_pseudo_types.contains(&canonical.as_str()) {
            if array_dimensions != 0 {
                return Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("type `{type_name}` does not exist"),
                });
            }
            return Ok(canonical);
        }
        ColumnType::from_sql_name(base).map_err(|error| match error {
            SQLError::Unsupported(_) => {
                SQLError::Unsupported(format!("routine type `{type_name}` is not implemented"))
            }
            other => other,
        })?
    };
    let mut resolved = resolved;
    for _ in 0..array_dimensions {
        resolved = ColumnType::Array(Box::new(resolved));
    }
    Ok(resolved.sql_name())
}

fn resolve_plpgsql_datum_types(
    engine: &Engine,
    function: &mut uqa_sql::plpgsql::PLpgSQLFunction,
) -> Result<(), SQLError> {
    for datum in &mut function.datums {
        let uqa_sql::plpgsql::PLpgSQLDatum::Var(variable) = datum else {
            continue;
        };
        variable.type_name = resolve_routine_type_name_with_reference(
            engine,
            &variable.type_name,
            &[
                "record",
                "refcursor",
                "anyelement",
                "anyarray",
                "anynonarray",
                "anyenum",
                "anyrange",
                "anymultirange",
                "anycompatible",
                "anycompatiblearray",
                "anycompatiblenonarray",
                "anycompatiblerange",
                "anycompatiblemultirange",
            ],
            variable.type_reference.as_ref(),
        )?;
        variable.type_reference = None;
    }
    Ok(())
}

fn validate_routine_declaration(engine: &Engine, def: &CreateFunction) -> Result<(), SQLError> {
    validate_variadic_declaration(engine, def)?;
    let inputs = validate_routine_input_types(def)?;
    if matches!(def.body, FunctionBody::Statements(_)) && inputs.any {
        return Err(routine_definition_error(
            "SQL function with unquoted function body cannot have polymorphic arguments",
        ));
    }
    validate_routine_output_types(def, &inputs)
}

fn validate_variadic_declaration(engine: &Engine, def: &CreateFunction) -> Result<(), SQLError> {
    let variadic_positions = def
        .params
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| {
            (parameter.mode == FunctionParamMode::Variadic).then_some(index)
        })
        .collect::<Vec<_>>();
    if variadic_positions.len() > 1 {
        return Err(routine_definition_error(
            "VARIADIC parameter must be the last parameter",
        ));
    }
    if let Some(&variadic_index) = variadic_positions.first() {
        let parameter = &def.params[variadic_index];
        if !routine_declaration_is_array(engine, &parameter.type_name) {
            return Err(routine_definition_error(
                "VARIADIC parameter must be an array",
            ));
        }
        let has_later_input = def.params[variadic_index + 1..].iter().any(|parameter| {
            matches!(
                parameter.mode,
                FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic
            )
        });
        if has_later_input || def.is_procedure && variadic_index + 1 != def.params.len() {
            return Err(routine_definition_error(
                "VARIADIC parameter must be the last parameter",
            ));
        }
    }
    Ok(())
}

#[derive(Default)]
struct PolymorphicInputs {
    simple: bool,
    compatible: bool,
    any: bool,
}

fn validate_routine_input_types(def: &CreateFunction) -> Result<PolymorphicInputs, SQLError> {
    let mut inputs = PolymorphicInputs::default();
    for parameter in &def.params {
        let type_name = canonical_routine_type_name(&parameter.type_name);
        let is_input = matches!(
            parameter.mode,
            FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic
        );
        if let Some(family) = polymorphic_family(&type_name) {
            inputs.any |= is_input;
            if is_input {
                match family {
                    RoutinePolymorphicFamily::Simple => inputs.simple = true,
                    RoutinePolymorphicFamily::Compatible => inputs.compatible = true,
                }
            }
            continue;
        }
        if ROUTINE_PARAMETER_PSEUDO_TYPES.contains(&type_name.as_str()) {
            let supported = match type_name.as_str() {
                "record" => !is_input || def.language == "plpgsql",
                "refcursor" => true,
                _ => false,
            };
            if !supported {
                return Err(routine_definition_error(format!(
                    "{} routines cannot have arguments of type {type_name}",
                    def.language
                )));
            }
        }
    }
    Ok(inputs)
}

fn validate_routine_output_types(
    def: &CreateFunction,
    inputs: &PolymorphicInputs,
) -> Result<(), SQLError> {
    let mut output_types = def
        .output_params()
        .into_iter()
        .map(|parameter| parameter.type_name.as_str())
        .collect::<Vec<_>>();
    if let FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } =
        &def.returns
    {
        output_types.push(type_name);
    }
    for output_type in output_types {
        let type_name = canonical_routine_type_name(output_type);
        match polymorphic_family(&type_name) {
            Some(RoutinePolymorphicFamily::Simple) if !inputs.simple => {
                return Err(routine_definition_error(format!(
                    "cannot determine result data type: a result of type {type_name} requires at least one simple polymorphic input"
                )));
            }
            Some(RoutinePolymorphicFamily::Compatible) if !inputs.compatible => {
                return Err(routine_definition_error(format!(
                    "cannot determine result data type: a result of type {type_name} requires at least one compatible polymorphic input"
                )));
            }
            None if ROUTINE_RESULT_PSEUDO_TYPES.contains(&type_name.as_str())
                && !matches!(type_name.as_str(), "record" | "refcursor" | "void")
                && !(type_name == "trigger"
                    && def.language == "plpgsql"
                    && !def.is_procedure
                    && def.params.is_empty()
                    && matches!(def.returns, FunctionReturns::Scalar { .. })) =>
            {
                return Err(routine_definition_error(format!(
                    "{} routines cannot return type {type_name}",
                    def.language
                )));
            }
            Some(_) | None => {}
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoutinePolymorphicFamily {
    Simple,
    Compatible,
}

fn polymorphic_family(type_name: &str) -> Option<RoutinePolymorphicFamily> {
    if !POLYMORPHIC_PSEUDO_TYPES.contains(&type_name) {
        return None;
    }
    Some(if type_name.starts_with("anycompatible") {
        RoutinePolymorphicFamily::Compatible
    } else {
        RoutinePolymorphicFamily::Simple
    })
}

fn routine_declaration_is_array(engine: &Engine, type_name: &str) -> bool {
    let canonical = canonical_routine_type_name(type_name);
    canonical.ends_with("[]")
        || matches!(
            canonical.as_str(),
            "anyarray" | "anycompatiblearray" | "int2vector" | "oidvector"
        )
        || crate::sql::resolve_catalog_column_type(engine, &canonical)
            .is_some_and(|ty| routine_column_type_is_array(&ty))
}

fn routine_column_type_is_array(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Array(_) | ColumnType::AnyArray => true,
        ColumnType::Domain { base, .. } => routine_column_type_is_array(base),
        _ => false,
    }
}

fn routine_definition_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P13".into(),
        message: message.into(),
    }
}

/// Compile a routine body per its language. Shared by DDL
/// registration and restore-from-catalog.
pub(super) fn compile_function_body(
    engine: &Engine,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_function_body_inner(engine, def, false)
}

pub(super) fn compile_persisted_function_body(
    engine: &Engine,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_function_body_inner(engine, def, true)
}

fn compile_function_body_inner(
    engine: &Engine,
    def: &CreateFunction,
    upgrade_legacy_dispatches: bool,
) -> Result<CompiledFunctionBody, SQLError> {
    validate_routine_declaration(engine, def)?;
    match def.language.as_str() {
        "plpgsql" => {
            if matches!(def.body, FunctionBody::Statements(_)) {
                return Err(SQLError::Unsupported(
                    "LANGUAGE plpgsql with a SQL-standard body".into(),
                ));
            }
            let mut function = uqa_sql::plpgsql::parse_function(def)?;
            resolve_plpgsql_datum_types(engine, &mut function)?;
            Ok(CompiledFunctionBody::PLpgSQL(function))
        }
        "sql" => {
            let (statements, bind_catalog_dependencies) = match &def.body {
                FunctionBody::Source(source) => (uqa_sql::compile(source)?, false),
                FunctionBody::Statements(statements) => (statements.clone(), true),
            };
            Ok(CompiledFunctionBody::SQL(compile_sql_routine_plans(
                engine,
                def,
                statements,
                bind_catalog_dependencies,
                upgrade_legacy_dispatches && matches!(def.body, FunctionBody::Statements(_)),
            )?))
        }
        other => Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("language \"{other}\" does not exist"),
        }),
    }
}

fn compile_sql_routine_plans(
    engine: &Engine,
    def: &CreateFunction,
    statements: Vec<Statement>,
    bind_catalog_dependencies: bool,
    upgrade_legacy_dispatches: bool,
) -> Result<Vec<UnifiedPlan>, SQLError> {
    let local_name = routine_local_name(&def.name)?;
    let signature_params = def.signature_params();
    let parameter_names: Vec<String> = signature_params
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect();
    let parameter_types = signature_params
        .iter()
        .map(|parameter| {
            crate::sql::resolve_catalog_column_type(engine, &parameter.type_name)
                .or_else(|| ColumnType::from_sql_name(&parameter.type_name).ok())
        })
        .collect::<Vec<_>>();
    let positional_parameters = parameter_types
        .iter()
        .map(|parameter_type| match parameter_type {
            Some(parameter_type) => {
                uqa_sql::SQLParam::typed_scalar(crate::Value::Null, parameter_type.clone())
            }
            None => uqa_sql::SQLParam::scalar(crate::Value::Null),
        })
        .collect::<Vec<_>>();
    let parameter_scope = uqa_execution::RowSchema::with_qualified_types(
        &local_name,
        parameter_names.clone(),
        parameter_types,
    );
    statements
        .into_iter()
        .map(|statement| {
            let mut plan = UnifiedPlan::lower_with(statement, &|name: &str| {
                engine.has_registered_aggregate_function(name)
            });
            if upgrade_legacy_dispatches {
                plan.rewrite_scalar_expressions(&mut |expression| {
                    let ScalarExpr::Func { name, binding, .. } = expression else {
                        return;
                    };
                    uqa_sql::ast::FunctionBinding::upgrade_legacy_serialized_dispatch(
                        name, binding,
                    );
                });
            }
            if bind_catalog_dependencies {
                if let UnifiedPlan::Query(query) = &mut plan {
                    crate::sql::bind_catalog_query_routines_with_outer(
                        engine,
                        query,
                        &positional_parameters,
                        &parameter_scope,
                    )?;
                }
            }
            plan.rewrite_scalar_expressions(&mut |expression| {
                let parameter = match expression {
                    ScalarExpr::Column(name) => parameter_names
                        .iter()
                        .position(|parameter| !parameter.is_empty() && parameter == name),
                    ScalarExpr::QualifiedColumn {
                        qualifier, column, ..
                    } if qualifier == &local_name => parameter_names
                        .iter()
                        .position(|parameter| !parameter.is_empty() && parameter == column),
                    _ => None,
                };
                if let Some(position) = parameter {
                    *expression = ScalarExpr::Param(position + 1);
                }
            });
            crate::sql::optimize_engine_plan(engine, plan)
        })
        .collect()
}
