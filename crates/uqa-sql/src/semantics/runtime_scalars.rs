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
    if !args.is_empty() {
        return Err(SQLError::BadArity {
            name: "merge_action".into(),
            expected: "0".into(),
            actual: args.len(),
        });
    }
    let action = row
        .internal_column(super::merge_action_attribute())
        .cloned()
        .ok_or_else(|| {
            SQLError::Unsupported("merge_action() is only valid in MERGE RETURNING".into())
        })?;
    Ok(action)
}
