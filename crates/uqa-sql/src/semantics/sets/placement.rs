//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Where a set-returning call may appear, and the diagnostics `PostgreSQL` reports for one that may not.

use crate::SQLError;

const LATERAL_HINT: &str =
    "You might be able to move the set-returning function into a LATERAL FROM item.";

/// The kind of expression under analysis, as `PostgreSQL`'s `check_srf_call_placement` distinguishes them by `ParseExprKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetPlacement {
    /// A target list entry or a clause processed like one: GROUP BY, ORDER BY, DISTINCT ON, a window's PARTITION BY and ORDER BY, and the single row of `INSERT ... VALUES` or of a MERGE INSERT action. A set-returning call there expands the rows.
    Expanding,
    /// An argument of a function in FROM, which may not itself contain a set-returning call.
    FromFunction,
    JoinCondition,
    Where,
    Having,
    Filter,
    /// A window frame offset.
    WindowFrame,
    /// An UPDATE source or target, including `ON CONFLICT DO UPDATE SET` and a MERGE UPDATE action.
    Update,
    Limit,
    Offset,
    Returning,
    /// A row of a multiple-row `VALUES` list, or of `VALUES` used as a query or in FROM.
    Values,
    MergeWhen,
    /// An `ON CONFLICT` inference expression.
    IndexExpression,
    /// An `ON CONFLICT` inference predicate.
    IndexPredicate,
    ExecuteParameter,
    TriggerWhen,
    CallArgument,
}

impl SetPlacement {
    /// Admit a set-returning call in this kind of expression; `nested` reports whether the call's own arguments contain one.
    pub fn admit(self, nested: bool) -> Result<(), SQLError> {
        let context = match self {
            Self::Expanding => return Ok(()),
            Self::FromFunction if nested => return Err(top_level_from()),
            Self::FromFunction => return Ok(()),
            Self::JoinCondition => "JOIN conditions",
            Self::Where => "WHERE",
            Self::Having => "HAVING",
            Self::Filter => "FILTER",
            Self::WindowFrame => "window definitions",
            Self::Update => "UPDATE",
            Self::Limit => "LIMIT",
            Self::Offset => "OFFSET",
            Self::Returning => "RETURNING",
            Self::Values => "VALUES",
            Self::MergeWhen => "MERGE WHEN conditions",
            Self::IndexExpression => "index expressions",
            Self::IndexPredicate => "index predicates",
            Self::ExecuteParameter => "EXECUTE parameters",
            Self::TriggerWhen => "trigger WHEN conditions",
            Self::CallArgument => "CALL arguments",
        };
        Err(feature_not_supported(format!(
            "set-returning functions are not allowed in {context}"
        )))
    }
}

/// A set-returning call nested in a function in FROM.
pub fn top_level_from() -> SQLError {
    feature_not_supported("set-returning functions must appear at top level of FROM".into())
}

/// A construct that evaluates its inputs conditionally or once per group or window, and so cannot contain a set-returning call: `CASE` and `COALESCE`, and the arguments of an aggregate or window function call.
pub fn conditional_set_error(message: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: message.into(),
        detail: None,
        hint: Some(LATERAL_HINT.into()),
    }
}

/// A set-valued operand of a construct that requires a single value, as `coerce_to_boolean` reports it.
pub fn set_argument_error(construct: &str) -> SQLError {
    datatype_mismatch(format!("argument of {construct} must not return a set"))
}

/// A row comparison, or the comparison of a subquery test expression, whose operator returns a set.
pub fn row_comparison_set_error() -> SQLError {
    datatype_mismatch("row comparison operator must not return a set".into())
}

/// A set-returning call reached an expression evaluated for one value, as `PostgreSQL`'s executor reports it when it initializes such an expression.
pub fn set_valued_call_error() -> SQLError {
    feature_not_supported("set-valued function called in context that cannot accept a set".into())
}

fn feature_not_supported(message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: "0A000".into(),
        message,
    }
}

fn datatype_mismatch(message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: "42804".into(),
        message,
    }
}
