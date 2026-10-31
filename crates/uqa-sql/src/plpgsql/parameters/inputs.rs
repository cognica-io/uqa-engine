//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation parameters retain selected values, types and lazy diagnostics under their producer.

use super::{PLpgSQLVariableReference, VariableResolver};
use crate::{ast::DeferredSQLError, ColumnType, SQLError, SQLParam};
use uqa_core::memory::{Produced, ProductionControl};

impl PLpgSQLVariableReference {
    pub fn read_with_control(
        &self,
        resolver: &mut dyn VariableResolver,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<SQLParam>, SQLError> {
        let input = resolver
            .resolve_input_with_control(self, control)?
            .ok_or_else(|| {
                SQLError::Internal("prepared PL/pgSQL variable no longer resolves".into())
            })?;
        let (input, memory) = input.into_parts();
        Ok(control.finish(input.parameter, memory)?)
    }
}

pub fn parameter_type_mismatch_with_control(
    number: usize,
    actual: &ColumnType,
    expected: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<DeferredSQLError>, SQLError> {
    let actual = actual.display_name_with_control(control)?;
    let expected = expected.display_name_with_control(control)?;
    let (sqlstate, first) = control.copy_text("42804")?.into_parts();
    let (message, second) = control
        .format(format_args!(
            "type of parameter {number} ({}) does not match that when preparing the plan ({})",
            actual.as_str(),
            expected.as_str()
        ))?
        .into_parts();
    Ok(control.finish(
        DeferredSQLError {
            sqlstate,
            message,
            detail: None,
            hint: None,
        },
        control.combine(first, second),
    )?)
}

impl DeferredSQLError {
    pub fn capture_with_control(
        error: &SQLError,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let (sqlstate, mut memory) = control
            .copy_text(error.sqlstate().unwrap_or("XX000"))?
            .into_parts();
        let (message, lease) = control.format(format_args!("{error}"))?.into_parts();
        memory = control.combine(memory, lease);
        let mut optional = |value: Option<&str>| -> Result<Option<String>, SQLError> {
            value
                .map(|value| {
                    let (value, lease) = control.copy_text(value)?.into_parts();
                    memory = control.combine(memory.take(), lease);
                    Ok(value)
                })
                .transpose()
        };
        let detail = optional(error.detail())?;
        let hint = optional(error.hint())?;
        Ok(control.finish(
            Self {
                sqlstate,
                message,
                detail,
                hint,
            },
            memory,
        )?)
    }
}

impl SQLParam {
    pub fn deferred_error_with_control(
        ty: &ColumnType,
        error: Produced<DeferredSQLError>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let (ty, memory) = ty.clone_with_control(control)?.into_parts();
        let (error, lease) = error.into_parts();
        let memory = control.combine(memory, lease);
        let memory = control.combine(memory, control.reserve(size_of::<DeferredSQLError>())?);
        Ok(control.finish(
            Self::DeferredError {
                ty,
                error: Box::new(error),
            },
            memory,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn deferred_inputs_keep_diagnostics_lazy_and_release_selected_owners() {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let error = parameter_type_mismatch_with_control(
            3,
            &ColumnType::Text,
            &ColumnType::Integer,
            &control,
        )
        .unwrap();
        assert_eq!(
            *error,
            crate::plpgsql::parameter_type_mismatch(3, &ColumnType::Text, &ColumnType::Integer)
        );
        let parameter =
            SQLParam::deferred_error_with_control(&ColumnType::Integer, error, &control).unwrap();
        assert_eq!(parameter.declared_scalar_type(), Some(&ColumnType::Integer));
        assert_eq!(parameter.to_value().unwrap_err().sqlstate(), Some("42804"));
        assert_eq!(budget.used(), parameter.reserved_bytes());
        assert!(budget.used() > 0);
        drop(parameter);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn retained_diagnostics_admit_each_destination_and_observe_both_cancellations() {
        let error = SQLError::Diagnostic {
            sqlstate: "42703".into(),
            message: "x".repeat(65536),
            detail: Some("detail".into()),
            hint: Some("hint".into()),
        };
        for cancelled in 0..3 {
            let budget = MemoryBudget::new(if cancelled == 0 { 32 } else { 1 << 20 });
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if cancelled == 1 {
                original.cancel();
            }
            if cancelled == 2 {
                invoking.cancel();
            }
            let control = ProductionControl::new(&budget, &original, &invoking);
            let observed = allocation_counter::measure(|| {
                assert_eq!(
                    DeferredSQLError::capture_with_control(&error, &control)
                        .err()
                        .unwrap()
                        .sqlstate(),
                    Some(if cancelled == 0 { "53200" } else { "57014" })
                );
            });
            assert!(observed.bytes_total < 4096, "{observed:?}");
            assert_eq!(budget.used(), 0);
        }
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let diagnostic = DeferredSQLError::capture_with_control(&error, &control).unwrap();
        assert_eq!(*diagnostic, DeferredSQLError::from(&error));
        assert_eq!(budget.used(), diagnostic.reserved_bytes());
        drop(diagnostic);
        assert_eq!(budget.used(), 0);
    }
}
