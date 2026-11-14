//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine declaration checks preserve language-specific precedence across admitted type-name work.

use super::{
    routine_definition_error, CreateFunction, FunctionParamMode, FunctionReturns, SQLError,
    POLYMORPHIC_PSEUDO_TYPES, ROUTINE_PARAMETER_PSEUDO_TYPES, ROUTINE_RESULT_PSEUDO_TYPES,
};
use crate::ast::FunctionBody;
use std::future::Future;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl},
};

pub(super) fn validate_inline(def: &CreateFunction) -> Result<(), SQLError> {
    crate::catalog::work::inline(validate(def, &ProductionControl::uncontrolled(), &mut ()))
}

/// Validate declared pseudo-types and polymorphic families without compiling a body or resolving catalog names. Type names and parameter traversal share the invoking continuation and native scratch allowance.
pub async fn validate_routine_declaration_from_input(
    def: &CreateFunction,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    validate(def, control, input).await
}

async fn validate<W: Work>(
    def: &CreateFunction,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    checkpoint(control, work).await?;
    let inputs = inspect_parameter_types(def, control, work).await?;
    if matches!(def.body, FunctionBody::Statements(_)) && (inputs.simple || inputs.compatible) {
        return Err(routine_definition_error(
            "SQL function with unquoted function body cannot have polymorphic arguments",
        ));
    }
    let unsupported_result = validate_routine_output_types(def, &inputs, control, work).await?;
    if let Some(type_name) = unsupported_result {
        let message = format!("cannot return type {type_name}");
        return Err(pseudo_type_error(def, message.clone(), message));
    }
    if let Some(type_name) = inputs.unsupported {
        return Err(input_pseudo_type_error(def, type_name));
    }
    Ok(control.check()?)
}

trait Work: crate::catalog::work::Work + Send {
    fn canonical(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>, SQLError>> + Send;
}

impl Work for () {
    async fn canonical(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        Ok(crate::type_resolution::canonical_routine_type_name_with_control(name, control)?)
    }
}

impl Work for AsyncImageInput<'_> {
    async fn canonical(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        crate::type_resolution::canonical_routine_type_name_from_input(name, control, self)
            .await
            .map_err(|error| match error {
                crate::type_resolution::RoutineSignatureInputError::Native(error) => error.into(),
                crate::type_resolution::RoutineSignatureInputError::Work(error) => error.into(),
            })
    }
}

async fn checkpoint<W: Work>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    control.check()?;
    work.checkpoint().await?;
    Ok(control.check()?)
}

#[derive(Default)]
struct ParameterTypes<'a> {
    simple: bool,
    compatible: bool,
    internal: bool,
    unsupported: Option<&'static str>,
    outputs: OutputType<'a>,
}

#[derive(Default)]
enum OutputType<'a> {
    #[default]
    None,
    Single(&'a str),
    Record,
}

async fn inspect_parameter_types<'a, W: Work>(
    def: &'a CreateFunction,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<ParameterTypes<'a>, SQLError> {
    let mut inputs = ParameterTypes::default();
    for parameter in &def.params {
        checkpoint(control, work).await?;
        let canonical = work.canonical(&parameter.type_name, control).await?;
        let type_name = canonical.as_str();
        let is_input = matches!(
            parameter.mode,
            FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic
        );
        if matches!(
            parameter.mode,
            FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
        ) {
            inputs.outputs = match inputs.outputs {
                OutputType::None => OutputType::Single(&parameter.type_name),
                OutputType::Single(_) | OutputType::Record => OutputType::Record,
            };
        }
        inputs.internal |= is_input && type_name == "internal";
        if let Some(family) = polymorphic_family(type_name) {
            if is_input {
                match family {
                    RoutinePolymorphicFamily::Simple => inputs.simple = true,
                    RoutinePolymorphicFamily::Compatible => inputs.compatible = true,
                }
            }
            continue;
        }
        if let Some(pseudo_type) = ROUTINE_PARAMETER_PSEUDO_TYPES
            .iter()
            .copied()
            .find(|&name| name == type_name)
        {
            let supported = match type_name {
                "record" => !is_input || def.language == "plpgsql",
                "refcursor" => true,
                _ => false,
            };
            if !supported && (is_input || def.language == "plpgsql") {
                // Preserve the first argument error until return-signature and return-type validation complete.
                inputs.unsupported.get_or_insert(pseudo_type);
            }
        }
    }
    Ok(inputs)
}

fn input_pseudo_type_error(def: &CreateFunction, type_name: &str) -> SQLError {
    pseudo_type_error(
        def,
        format!("cannot have arguments of type {type_name}"),
        format!("cannot accept type {type_name}"),
    )
}

async fn validate_routine_output_types<W: Work>(
    def: &CreateFunction,
    inputs: &ParameterTypes<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<&'static str>, SQLError> {
    // Procedures return void or record. Functions infer their result from a single output parameter when RETURNS is absent.
    let result_type = if def.is_procedure {
        None
    } else {
        match &def.returns {
            FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
                Some(type_name.as_str())
            }
            FunctionReturns::Table | FunctionReturns::None => match inputs.outputs {
                OutputType::Single(type_name) => Some(type_name),
                OutputType::None | OutputType::Record => None,
            },
        }
    };
    let mut unsupported_result = None;
    if let Some(result_type) = result_type {
        let canonical = work.canonical(result_type, control).await?;
        let type_name = canonical.as_str();
        validate_result_signature(inputs, type_name)?;
        unsupported_result = ROUTINE_RESULT_PSEUDO_TYPES.iter().copied().find(|&name| {
            name == type_name
                && polymorphic_family(name).is_none()
                && !matches!(name, "record" | "refcursor" | "void")
                && !(matches!(name, "trigger" | "event_trigger") && def.language == "plpgsql")
        });
    }
    for parameter in &def.params {
        checkpoint(control, work).await?;
        if matches!(
            parameter.mode,
            FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
        ) {
            let canonical = work.canonical(&parameter.type_name, control).await?;
            validate_result_signature(inputs, canonical.as_str())?;
        }
    }
    Ok(unsupported_result)
}

fn validate_result_signature(inputs: &ParameterTypes<'_>, type_name: &str) -> Result<(), SQLError> {
    match polymorphic_family(type_name) {
        Some(RoutinePolymorphicFamily::Simple) if !inputs.simple => {
            return Err(polymorphic_result_error(
                type_name,
                "anyelement, anyarray, anynonarray, anyenum, anyrange, or anymultirange",
            ));
        }
        Some(RoutinePolymorphicFamily::Compatible) if !inputs.compatible => {
            return Err(polymorphic_result_error(
                type_name,
                "anycompatible, anycompatiblearray, anycompatiblenonarray, anycompatiblerange, or anycompatiblemultirange",
            ));
        }
        Some(_) | None => {}
    }
    if type_name == "internal" && !inputs.internal {
        return Err(SQLError::Diagnostic {
            sqlstate: "42P13".into(),
            message: "unsafe use of pseudo-type \"internal\"".into(),
            detail: Some(
                "A result of type internal requires at least one input of type internal.".into(),
            ),
            hint: None,
        });
    }
    Ok(())
}

fn polymorphic_result_error(result: &str, inputs: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42P13".into(),
        message: "cannot determine result data type".into(),
        detail: Some(format!(
            "A result of type {result} requires at least one input of type {inputs}."
        )),
        hint: None,
    }
}

/// A pseudo-type the routine's language rejects, reported as its validator reports it: `fmgr_sql_validator` as an invalid definition, and `plpgsql_validator` as an unsupported feature.
fn pseudo_type_error(def: &CreateFunction, sql: String, plpgsql: String) -> SQLError {
    if def.language == "plpgsql" {
        SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("PL/pgSQL functions {plpgsql}"),
        }
    } else {
        routine_definition_error(format!("SQL functions {sql}"))
    }
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

#[cfg(test)]
mod tests;
