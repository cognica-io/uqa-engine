//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime scalar argument and row-context rules.

use crate::{expr::RowLookup, SQLError, ScalarExpr};
use uqa_core::Value;

pub fn deep_learn_arguments(
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<(String, String), SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "deep_learn".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let model_name = match evaluate(&args[0])? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.model must be a string, got {other:?}"
            )));
        }
    };
    let training_source = match evaluate(&args[1])? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.training_set must be a table name or JSON string, got {other:?}"
            )));
        }
    };
    Ok((model_name, training_source))
}

pub enum DeepLearnSource<'a> {
    Json(&'a str),
    Table(&'a str),
}
pub fn deep_learn_source(source: &str) -> DeepLearnSource<'_> {
    let trimmed = source.trim();
    if trimmed.starts_with('{') {
        DeepLearnSource::Json(trimmed)
    } else {
        DeepLearnSource::Table(source)
    }
}
pub fn no_scalar_arguments(name: &str, arguments: &[Value]) -> Result<(), SQLError> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err(SQLError::BadArity {
            name: name.into(),
            expected: "0".into(),
            actual: arguments.len(),
        })
    }
}
pub fn notification_arguments(arguments: &[Value]) -> Result<(&str, &str), SQLError> {
    match arguments {
        [channel, payload] => Ok((
            notification_text_argument(channel, "channel")?,
            notification_text_argument(payload, "payload")?,
        )),
        _ => Err(SQLError::BadArity {
            name: "pg_notify".into(),
            expected: "2".into(),
            actual: arguments.len(),
        }),
    }
}
fn notification_text_argument<'a>(value: &'a Value, label: &str) -> Result<&'a str, SQLError> {
    match value {
        Value::Null => Ok(""),
        Value::Str(value) => Ok(value),
        Value::FixedChar(value) => Ok(value.trim_end_matches(' ')),
        other => Err(SQLError::TypeMismatch(format!(
            "pg_notify {label} must be text, got {other:?}"
        ))),
    }
}

pub fn merge_action_value(args: &[ScalarExpr], row: &dyn RowLookup) -> Result<Value, SQLError> {
    Ok(merge_action_value_with_control(
        args.len(),
        row,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary MERGE action output has no reservation"))
}

pub(crate) fn merge_action_value_with_control(
    argument_count: usize,
    row: &dyn RowLookup,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<Value>, SQLError> {
    control.check()?;
    if argument_count != 0 {
        return Err(SQLError::BadArity {
            name: "merge_action".into(),
            expected: "0".into(),
            actual: argument_count,
        });
    }
    let action = row
        .internal_column(super::merge_action_attribute())
        .ok_or_else(|| {
            SQLError::Unsupported("merge_action() is only valid in MERGE RETURNING".into())
        })?;
    Ok(control.copy_value(action)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::memory::{MemoryBudget, ProductionControl};
    use uqa_core::CancellationToken;

    struct ActionRow(Value);
    impl RowLookup for ActionRow {
        fn column(&self, _: &str) -> Option<&Value> {
            None
        }
        fn qualified_column(&self, _: &str, _: &str) -> Option<&Value> {
            None
        }
        fn internal_column(&self, column: crate::ast::InternalColumnRef) -> Option<&Value> {
            (column == crate::semantics::merge_action_attribute()).then_some(&self.0)
        }
    }

    #[test]
    fn merge_action_output_retains_its_allowance_and_honors_cancellation() {
        let row = ActionRow(Value::Str("UPDATE".into()));
        let budget = MemoryBudget::new(256);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let output = merge_action_value_with_control(0, &row, &control).unwrap();
        assert_eq!(*output, row.0);
        assert!(budget.used() > 0);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
        let tiny = MemoryBudget::new(0);
        let limited = ProductionControl::new(&tiny, &original, &invoking);
        assert_eq!(
            merge_action_value_with_control(0, &row, &limited)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(tiny.used(), 0);
        invoking.cancel();
        assert_eq!(
            merge_action_value_with_control(0, &row, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
