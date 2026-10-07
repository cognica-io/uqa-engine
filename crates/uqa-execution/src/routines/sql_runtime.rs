//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dynamic SQL, `RAISE`, and statement-result bookkeeping.

use super::{
    cast_value_from, coercion_type_name, compile, condition_sqlstate, format_raise_message,
    looks_like_sqlstate, result_row_count, result_row_values, strict_into_check, Flow, Interpreter,
    IntoTarget, PLpgSQLExpression, RaiseLevel, SQLError, SQLParam, SQLResult, Statement, Value,
};

impl Interpreter<'_> {
    pub(super) fn exec_assert(
        &mut self,
        condition: &PLpgSQLExpression,
        message: Option<&PLpgSQLExpression>,
    ) -> Result<Flow, SQLError> {
        if !self.services.statements.assertions_enabled() {
            return Ok(Flow::Normal);
        }
        if self.eval_boolean(condition)? == Some(true) {
            return Ok(Flow::Normal);
        }
        let message = match message {
            Some(expression) => {
                let (value, declared_type) = self.eval_expr_with_type(expression)?;
                let source_type = declared_type.as_ref().map(coercion_type_name);
                match cast_value_from(&value, "text", source_type.as_deref())? {
                    Value::Null => "assertion failed".into(),
                    Value::Str(text) => text,
                    other => {
                        return Err(SQLError::Internal(format!(
                            "PL/pgSQL ASSERT message coercion returned {other:?}"
                        )))
                    }
                }
            }
            None => "assertion failed".into(),
        };
        Err(SQLError::Routine {
            sqlstate: "P0004".into(),
            message,
        })
    }

    pub(super) fn exec_raise(
        &mut self,
        level: RaiseLevel,
        condition: Option<&str>,
        message: Option<&str>,
        params: &[PLpgSQLExpression],
        options: &[uqa_sql::plpgsql::RaiseOption],
    ) -> Result<Flow, SQLError> {
        // Bare RAISE re-throws the error being handled.
        if condition.is_none() && message.is_none() && options.is_empty() {
            return match self.err_stack.last() {
                Some(error) => Err(error.cause.clone()),
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
                Some(format_raise_message(format, &values)?)
            }
            None => None,
        };
        let sqlstate = match condition {
            Some(name) => Some(if let Some(state) = condition_sqlstate(name) {
                state.to_string()
            } else if looks_like_sqlstate(name) {
                name.to_ascii_uppercase()
            } else {
                return Err(SQLError::Internal(format!(
                    "unrecognized PL/pgSQL RAISE condition `{name}`"
                )));
            }),
            None => None,
        };
        let mut diagnostic = uqa_sql::plpgsql::RaiseDiagnostic {
            sqlstate,
            condition: condition.map(str::to_owned),
            message: text,
            ..Default::default()
        };
        for option in options {
            let (value, ty) = self.eval_expr_with_type(&option.value)?;
            if matches!(value, Value::Null) {
                return Err(SQLError::Routine {
                    sqlstate: "22004".into(),
                    message: "RAISE statement option cannot be null".into(),
                });
            }
            let text = match ty.as_ref() {
                Some(ty) => uqa_sql::result::format_postgres_text(
                    &value,
                    ty,
                    Some(self.services.expressions),
                )?,
                None => uqa_sql::plpgsql::runtime_diagnostics::raise_text(&value)?,
            };
            diagnostic.option(option.kind, text)?;
        }
        let sqlstate = diagnostic.sqlstate.filter(|state| state != "00000");
        let notice_level = level.notice_level();
        let text = diagnostic
            .message
            .or(diagnostic.condition)
            .unwrap_or_else(|| {
                sqlstate
                    .as_deref()
                    .unwrap_or(if notice_level.is_some() {
                        "00000"
                    } else {
                        "P0001"
                    })
                    .to_owned()
            });
        let Some(level) = notice_level else {
            if diagnostic.detail.is_none() && diagnostic.hint.is_none() {
                return Err(SQLError::Routine {
                    sqlstate: sqlstate.unwrap_or_else(|| "P0001".to_string()),
                    message: text,
                });
            }
            return Err(SQLError::Diagnostic {
                sqlstate: sqlstate.unwrap_or_else(|| "P0001".to_string()),
                message: text,
                detail: diagnostic.detail,
                hint: diagnostic.hint,
            });
        };
        let mut notice = uqa_sql::SQLNotice::new(level, text);
        if let Some(sqlstate) = sqlstate {
            notice = notice.with_sqlstate(sqlstate);
        }
        notice.detail = diagnostic.detail;
        notice.hint = diagnostic.hint;
        self.services.runtime.push_notice(notice);
        Ok(Flow::Normal)
    }

    pub(super) fn exec_dynamic(
        &mut self,
        query: &PLpgSQLExpression,
        params: &[PLpgSQLExpression],
    ) -> Result<SQLResult, SQLError> {
        let (text, bound_params) = self.eval_dynamic_sql(query, params)?;
        let (statements, parser) =
            uqa_sql::parser::with_settings(self.services.statements.parser_settings(), || {
                compile(&text)
            });
        let has_transaction = statements.as_ref().is_ok_and(|statements| {
            statements
                .iter()
                .any(|statement| matches!(statement, Statement::Transaction(_)))
        });
        // Accepted text is parsed by execute_text at its statement boundary.
        // A preflight rejection still exposes warnings that preceded the error.
        if statements.is_err() || has_transaction {
            for notice in parser.notices.iter() {
                self.services.runtime.push_notice(notice.clone());
            }
        }
        let _ = statements?;
        if has_transaction {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "EXECUTE of transaction commands is not implemented".into(),
            });
        }
        self.services.statements.execute_text(&text, &bound_params)
    }

    pub(super) fn eval_dynamic_sql(
        &self,
        query: &PLpgSQLExpression,
        params: &[PLpgSQLExpression],
    ) -> Result<(String, Vec<SQLParam>), SQLError> {
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
        Ok((text, bound_params))
    }

    /// Post-process an embedded SQL statement's result: `ROW_COUNT`,
    /// `FOUND`, and `INTO` assignment.
    pub(super) fn consume_statement_result(
        &mut self,
        is_call: bool,
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
            let values = result_row_values(result, 0);
            self.assign_into(
                target,
                &result.columns,
                &result.column_types,
                values.as_deref(),
            )?;
        }
        // CALL statements leave FOUND untouched.
        if !is_call {
            self.set_found(row_count > 0);
        }
        Ok(())
    }
}
