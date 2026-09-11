//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Argument validation and short-circuit semantics for score and highlight projections.

use crate::{expr::RowLookup, semantics::expect_column_name, SQLError, ScalarExpr};
use uqa_core::Value;

pub fn validate_score_projection_args(
    name: &str,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<(), SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "1..=2".into(),
            actual: args.len(),
        });
    }
    let query_idx = args.len() - 1;
    if args.len() == 2 {
        let _ = expect_column_name(&args[0], &format!("{name}.field"))?;
    }
    match evaluate(&args[query_idx])? {
        Value::Str(_) => Ok(()),
        other => Err(SQLError::TypeMismatch(format!(
            "{name}.query must be a string, got {other:?}"
        ))),
    }
}

pub struct HighlightArguments {
    pub text: String,
    pub query: String,
    pub start_tag: String,
    pub end_tag: String,
    pub max_fragments: usize,
    pub fragment_size: usize,
}

pub enum HighlightInput {
    Value(Value),
    Arguments(HighlightArguments),
}

/// Evaluate fields and options in order, preserving NULL short circuits before later arguments.
#[expect(
    clippy::too_many_lines,
    reason = "preserves ordered field and option evaluation"
)]
pub fn highlight_arguments(
    row: &dyn RowLookup,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<HighlightInput, SQLError> {
    if args.len() < 2 || args.len() > 6 {
        return Err(SQLError::BadArity {
            name: "uqa_highlight".into(),
            expected: "2..=6".into(),
            actual: args.len(),
        });
    }
    let text = match &args[0] {
        ScalarExpr::Column(c) => match row.column(c) {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Null) => return Ok(HighlightInput::Value(Value::Null)),
            Some(other) => format!("{other:?}"),
            None => return Ok(HighlightInput::Value(Value::Null)),
        },
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            match row.qualified_column(qualifier, column) {
                Some(Value::Str(s)) => s.clone(),
                Some(Value::Null) => return Ok(HighlightInput::Value(Value::Null)),
                Some(other) => format!("{other:?}"),
                None => return Ok(HighlightInput::Value(Value::Null)),
            }
        }
        other => match evaluate(other)? {
            Value::Str(s) => s,
            Value::Null => return Ok(HighlightInput::Value(Value::Null)),
            v => format!("{v:?}"),
        },
    };
    let query_str = match evaluate(&args[1])? {
        Value::Str(s) => s,
        Value::Null => return Ok(HighlightInput::Value(Value::Str(text))),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "uqa_highlight query must be string, got {other:?}"
            )));
        }
    };
    let start_tag = match args.get(2) {
        Some(e) => match evaluate(e)? {
            Value::Str(s) => s,
            Value::Null => "<b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight start_tag must be string, got {other:?}"
                )));
            }
        },
        None => "<b>".into(),
    };
    let end_tag = match args.get(3) {
        Some(e) => match evaluate(e)? {
            Value::Str(s) => s,
            Value::Null => "</b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight end_tag must be string, got {other:?}"
                )));
            }
        },
        None => "</b>".into(),
    };
    let max_fragments = match args.get(4) {
        Some(e) => match evaluate(e)? {
            Value::Int(n) if n >= 0 => usize::try_from(n).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "uqa_highlight max_fragments {n} exceeds the platform usize range"
                ))
            })?,
            Value::Null => 0,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight max_fragments must be non-negative integer, got {other:?}"
                )));
            }
        },
        None => 0,
    };
    let fragment_size = match args.get(5) {
        Some(e) => match evaluate(e)? {
            Value::Int(n) if n > 0 => usize::try_from(n).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "uqa_highlight fragment_size {n} exceeds the platform usize range"
                ))
            })?,
            Value::Null => 150,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight fragment_size must be positive integer, got {other:?}"
                )));
            }
        },
        None => 150,
    };
    Ok(HighlightInput::Arguments(HighlightArguments {
        text,
        query: query_str,
        start_tag,
        end_tag,
        max_fragments,
        fragment_size,
    }))
}
