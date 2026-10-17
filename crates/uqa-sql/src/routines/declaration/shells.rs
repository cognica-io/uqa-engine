//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Undefined type notices precede language validation without creating a callable value type.

use crate::{
    ast::{CreateFunction, FunctionParamMode, FunctionReturns, RoutineColumnTypeReference},
    routines::{
        compilation::{RoutineCompilationContext, RoutineParserCatalog},
        declaration::RoutineTypeCatalog,
    },
    SQLError, SQLNotice,
};

#[derive(Default)]
pub struct RoutineShellTypes {
    argument: Option<u32>,
    result: Option<u32>,
    output: Option<u32>,
    outputs: usize,
}

impl RoutineShellTypes {
    pub(super) fn argument(
        &mut self,
        context: &RoutineCompilationContext<'_>,
        language: &str,
        name: &str,
        reference: Option<&RoutineColumnTypeReference>,
        mode: FunctionParamMode,
    ) -> Result<Option<u32>, SQLError> {
        let oid = resolve(
            context.types,
            context.parsers,
            language,
            name,
            reference,
            false,
        )?;
        if self.argument.is_none() {
            self.argument = oid;
        }
        if matches!(
            mode,
            FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
        ) {
            self.outputs += 1;
            self.output = oid;
        }
        Ok(oid)
    }

    pub(super) fn result(
        &mut self,
        context: &RoutineCompilationContext<'_>,
        language: &str,
        name: &str,
        reference: Option<&RoutineColumnTypeReference>,
    ) -> Result<bool, SQLError> {
        self.result = resolve(
            context.types,
            context.parsers,
            language,
            name,
            reference,
            true,
        )?;
        Ok(self.result.is_some())
    }

    /// PL/pgSQL validates return and argument pseudotypes even with `check_function_bodies` disabled.
    pub fn reject_unsupported(
        &self,
        context: &RoutineCompilationContext<'_>,
        def: &CreateFunction,
    ) -> Result<(), SQLError> {
        if self.argument.is_none() && self.result.is_none() {
            return Ok(());
        }
        if let (Some(output), 1, FunctionReturns::Scalar { .. } | FunctionReturns::SetOf { .. }) =
            (self.output, self.outputs, &def.returns)
        {
            if self.result != Some(output) {
                return Err(super::routine_definition_error(format!(
                    "function result type must be {} because of OUT parameters",
                    context.types.format_type_oid(output)?
                )));
            }
        }
        super::validate_routine_declaration(def)?;
        let result = self.result.or_else(|| {
            (matches!(def.returns, FunctionReturns::None | FunctionReturns::Table)
                && self.outputs == 1)
                .then_some(self.output)
                .flatten()
        });
        if let Some((oid, operation)) = result
            .map(|oid| (oid, "return"))
            .or_else(|| self.argument.map(|oid| (oid, "accept")))
        {
            let name = context.types.format_type_oid(oid)?;
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("PL/pgSQL functions cannot {operation} type {name}"),
            });
        }
        Ok(())
    }
}

fn resolve(
    types: &dyn RoutineTypeCatalog,
    parsers: &dyn RoutineParserCatalog,
    language: &str,
    name: &str,
    reference: Option<&RoutineColumnTypeReference>,
    result: bool,
) -> Result<Option<u32>, SQLError> {
    // A column already has a value type; its structured reference is resolved by declaration binding.
    if reference.is_some() {
        return Ok(None);
    }
    let Some(oid) = types.shell_type_oid(name)? else {
        return Ok(None);
    };
    let parsed = crate::parse_regtype_name(name)?
        .ok_or_else(|| SQLError::Internal("shell routine type has no name".into()))?;
    let written = parsed.names.join(".");
    if language == "sql" {
        let operation = if result { "return" } else { "accept" };
        return Err(super::routine_definition_error(format!(
            "SQL function cannot {operation} shell type {written}"
        )));
    }
    let position = if result { "return" } else { "argument" };
    parsers.parser_notice(
        SQLNotice::notice(format!("{position} type {written} is only a shell"))
            .with_sqlstate("42809"),
    );
    types.require_type_oid_usage(oid)?;
    Ok(Some(oid))
}

#[cfg(test)]
mod tests;
