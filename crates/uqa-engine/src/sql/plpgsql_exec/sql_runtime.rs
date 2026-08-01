//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dynamic SQL, `RAISE`, and statement-result bookkeeping.

use super::{
    condition_sqlstate, format_raise_message, looks_like_sqlstate, result_row_count,
    strict_into_check, Expr, Flow, Interpreter, IntoTarget, RaiseLevel, SQLError, SQLParam,
    SQLResult, Statement, Value,
};

impl Interpreter<'_> {
    pub(super) fn exec_raise(
        &mut self,
        level: RaiseLevel,
        condition: Option<&str>,
        message: Option<&str>,
        params: &[Expr],
    ) -> Result<Flow, SQLError> {
        // Bare RAISE re-throws the error being handled.
        if condition.is_none() && message.is_none() {
            return match self.err_stack.last() {
                Some((state, message)) => Err(SQLError::Routine {
                    sqlstate: state.clone(),
                    message: message.clone(),
                }),
                None => Err(SQLError::Routine {
                    sqlstate: "0Z002".into(),
                    message: "RAISE without parameters cannot be used outside an exception handler"
                        .into(),
                }),
            };
        }
        let text = match message {
            Some(format) => {
                let mut values = Vec::with_capacity(params.len());
                for param in params {
                    values.push(self.eval_expr(param)?);
                }
                format_raise_message(format, &values)?
            }
            None => condition
                .ok_or_else(|| {
                    SQLError::Internal(
                        "non-bare PL/pgSQL RAISE has neither condition nor message".into(),
                    )
                })?
                .to_string(),
        };
        if level == RaiseLevel::Error {
            let sqlstate = match condition {
                Some(name) => {
                    if let Some(state) = condition_sqlstate(name) {
                        state.to_string()
                    } else if looks_like_sqlstate(name) {
                        name.to_ascii_uppercase()
                    } else {
                        return Err(SQLError::Internal(format!(
                            "unrecognized PL/pgSQL RAISE condition `{name}`"
                        )));
                    }
                }
                None => "P0001".to_string(),
            };
            return Err(SQLError::Routine {
                sqlstate,
                message: text,
            });
        }
        self.engine.push_sql_notice(level.as_str(), &text);
        Ok(Flow::Normal)
    }

    pub(super) fn exec_dynamic(
        &mut self,
        query: &Expr,
        params: &[Expr],
    ) -> Result<SQLResult, SQLError> {
        let text = match self.eval_expr(query)? {
            Value::Str(text) => text,
            Value::Null => {
                return Err(SQLError::Routine {
                    sqlstate: "22004".into(),
                    message: "query string argument of EXECUTE is null".into(),
                });
            }
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "EXECUTE expects a query string, got {other:?}"
                )));
            }
        };
        let mut bound_params = Vec::with_capacity(params.len());
        for param in params {
            bound_params.push(SQLParam::Scalar(self.eval_expr(param)?));
        }
        crate::sql::execute(self.engine, &text, &bound_params)
    }

    /// Post-process an embedded SQL statement's result: `ROW_COUNT`,
    /// `FOUND`, and `INTO` assignment.
    pub(super) fn consume_statement_result(
        &mut self,
        statement: &Statement,
        result: &SQLResult,
        into: Option<&IntoTarget>,
        strict: bool,
    ) -> Result<(), SQLError> {
        let row_count = result_row_count(result)?;
        self.last_row_count = row_count;
        if let Some(target) = into {
            if strict {
                strict_into_check(row_count)?;
            }
            self.assign_into(target, &result.columns, result.rows.first())?;
        }
        // CALL statements leave FOUND untouched.
        if !matches!(statement, Statement::Call { .. }) {
            self.set_found(row_count > 0);
        }
        Ok(())
    }
}
