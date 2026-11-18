//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Application lookup sites in stored routine defaults and SQL-standard bodies.

use crate::{
    ast::{CreateFunction, FunctionBody},
    binding::view_dependencies::{
        visit_statement_runtime_calls, visit_stored_runtime_calls_from, StoredFunctionSite,
    },
    routines::CompiledFunctionBody,
    SQLError,
};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl, CancellationToken};

/// Visit retained executable statements when present, or the same SQL placements lowered from the persisted declaration. Runtime compilation of source text belongs to the invoking session. Routine argument defaults use the standalone catalog expression evaluator.
pub async fn visit_stored_routine_runtime_calls<'a, 'input: 'a, F>(
    definition: &'a CreateFunction,
    compiled: Option<&'a CompiledFunctionBody>,
    original: &'a CancellationToken,
    input: &'a mut AsyncImageInput<'input>,
    visit: &'a mut F,
) -> Result<(), SQLError>
where
    F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send,
{
    let control = ProductionControl::new(input.memory, original, input.cancellation);
    for parameter in &definition.params {
        input.checkpoint().await?;
        if let Some(default) = &parameter.default {
            default
                .visit_runtime_scalar_calls(original, input, visit)
                .await?;
        }
    }
    let FunctionBody::Statements(statements) = &definition.body else {
        return Ok(());
    };
    if let Some(body) = compiled {
        let CompiledFunctionBody::SQL(plans) = body else {
            return Err(SQLError::Internal(
                "invalid native routine body: session-compiled body".into(),
            ));
        };
        for plan in plans {
            input.checkpoint().await?;
            visit_stored_runtime_calls_from(plan.into(), &control, input, visit).await?;
        }
    } else {
        for statement in statements {
            input.checkpoint().await?;
            super::super::compilation::validate_sql_standard_statement(statement)?;
            visit_statement_runtime_calls(statement, &control, input, visit).await?;
        }
    }
    Ok(control.check()?)
}

#[cfg(test)]
mod tests;
