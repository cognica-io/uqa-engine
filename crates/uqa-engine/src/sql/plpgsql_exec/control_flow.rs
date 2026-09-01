//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Loop and return control-flow execution.

use super::{
    coerce_routine_value, result_row_values, to_i64_value, Expr, Flow, FunctionReturns,
    Interpreter, LoopSignal, PLpgSQLReturnValue, PLpgSQLStmt, SQLError, SQLResult, Value,
};

impl Interpreter<'_> {
    /// Run one loop iteration body and classify the resulting flow
    /// with respect to this loop's label.
    pub(super) fn exec_loop_body(
        &mut self,
        label: Option<&str>,
        body: &[PLpgSQLStmt],
    ) -> Result<LoopSignal, SQLError> {
        match self.exec_stmts(body)? {
            Flow::Normal => Ok(LoopSignal::Continue),
            Flow::Continue(flow_label) => {
                if flow_label.is_none() || flow_label.as_deref() == label {
                    Ok(LoopSignal::Continue)
                } else {
                    Ok(LoopSignal::Propagate(Flow::Continue(flow_label)))
                }
            }
            Flow::Exit(flow_label) => {
                if flow_label.is_none() || flow_label.as_deref() == label {
                    Ok(LoopSignal::Break)
                } else {
                    Ok(LoopSignal::Propagate(Flow::Exit(flow_label)))
                }
            }
            Flow::Return => Ok(LoopSignal::Propagate(Flow::Return)),
        }
    }

    #[expect(clippy::too_many_arguments, reason = "keeps PL/pgSQL context aligned")]
    pub(super) fn exec_fori(
        &mut self,
        label: Option<&str>,
        var: usize,
        lower: &Expr,
        upper: &Expr,
        step: Option<&Expr>,
        reverse: bool,
        body: &[PLpgSQLStmt],
    ) -> Result<Flow, SQLError> {
        let lower = self.eval_loop_bound(lower, "lower")?;
        let upper = self.eval_loop_bound(upper, "upper")?;
        let step_value = match step {
            Some(expr) => {
                let value = self.eval_expr(expr)?;
                if matches!(value, Value::Null) {
                    return Err(SQLError::Routine {
                        sqlstate: "22004".into(),
                        message: "BY value of FOR loop cannot be null".into(),
                    });
                }
                to_i64_value(&value)?
            }
            None => 1,
        };
        if step_value <= 0 {
            return Err(SQLError::Routine {
                sqlstate: "22023".into(),
                message: "BY value of FOR loop must be greater than zero".into(),
            });
        }
        let mut current = lower;
        let mut iterated = false;
        let mut outcome = Flow::Normal;
        loop {
            let done = if reverse {
                current < upper
            } else {
                current > upper
            };
            if done {
                break;
            }
            iterated = true;
            self.values[var] = Value::Int(current);
            match self.exec_loop_body(label, body)? {
                LoopSignal::Continue => {}
                LoopSignal::Break => break,
                LoopSignal::Propagate(flow) => {
                    outcome = flow;
                    break;
                }
            }
            if reverse {
                current -= step_value;
            } else {
                current += step_value;
            }
        }
        self.set_found(iterated);
        Ok(outcome)
    }

    pub(super) fn eval_loop_bound(&self, expr: &Expr, which: &str) -> Result<i64, SQLError> {
        let value = self.eval_expr(expr)?;
        if matches!(value, Value::Null) {
            return Err(SQLError::Routine {
                sqlstate: "22004".into(),
                message: format!("{which} bound of FOR loop cannot be null"),
            });
        }
        to_i64_value(&value)
    }

    pub(super) fn exec_return(
        &mut self,
        value: Option<&PLpgSQLReturnValue>,
    ) -> Result<Flow, SQLError> {
        if let Some(value) = value {
            let is_expression = matches!(value, PLpgSQLReturnValue::Expr(_));
            let context = if is_expression && self.def.is_procedure {
                Some("RETURN cannot have a parameter in a procedure")
            } else if is_expression && self.is_set {
                Some("RETURN cannot have a parameter in function returning set")
            } else if is_expression && !self.out_datums.is_empty() {
                Some("RETURN cannot have a parameter in function with OUT parameters")
            } else if is_expression
                && matches!(
                &self.def.returns,
                FunctionReturns::Scalar { type_name } if type_name == "void"
                )
            {
                Some("RETURN cannot have a parameter in function returning void")
            } else {
                None
            };
            if let Some(message) = context {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: message.into(),
                });
            }
            if self.def.is_procedure || self.is_set || !self.out_datums.is_empty() {
                return Ok(Flow::Return);
            }
            let value = self.eval_return_value(value)?;
            self.ret = match &self.def.returns {
                FunctionReturns::Scalar { type_name } => {
                    coerce_routine_value(self.engine, &value, type_name)?
                }
                _ => value,
            };
            return Ok(Flow::Return);
        }
        // Bare RETURN (including the implicit one the parser appends
        // at the end of every body). A plain scalar function reaching
        // it has produced no value - PostgreSQL's runtime error.
        let returns_void = matches!(
            &self.def.returns,
            FunctionReturns::Scalar { type_name } if type_name == "void"
        );
        let implicit_ok = self.def.is_procedure
            || self.is_set
            || returns_void
            || !self.out_datums.is_empty()
            || matches!(self.def.returns, FunctionReturns::None);
        if !implicit_ok {
            return Err(SQLError::Routine {
                sqlstate: "2F005".into(),
                message: "control reached end of function without RETURN".into(),
            });
        }
        Ok(Flow::Return)
    }

    pub(super) fn exec_return_next(
        &mut self,
        value: Option<&PLpgSQLReturnValue>,
    ) -> Result<Flow, SQLError> {
        if !self.is_set {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "cannot use RETURN NEXT in a non-SETOF function".into(),
            });
        }
        if !self.out_datums.is_empty() {
            if matches!(value, Some(PLpgSQLReturnValue::Expr(_))) {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: "RETURN NEXT cannot have a parameter in function with OUT parameters"
                        .into(),
                });
            }
            let row = self
                .out_datums
                .iter()
                .map(|idx| self.values[*idx].clone())
                .collect();
            self.set_rows.push(row);
            return Ok(Flow::Normal);
        }
        let Some(value) = value else {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "RETURN NEXT must have a parameter".into(),
            });
        };
        let value = self.eval_return_value(value)?;
        let value = match &self.def.returns {
            FunctionReturns::SetOf { type_name } => {
                coerce_routine_value(self.engine, &value, type_name)?
            }
            _ => value,
        };
        self.set_rows.push(vec![value]);
        Ok(Flow::Normal)
    }

    fn eval_return_value(&self, value: &PLpgSQLReturnValue) -> Result<Value, SQLError> {
        match value {
            PLpgSQLReturnValue::Expr(expr) => self.eval_expr(expr),
            PLpgSQLReturnValue::Datum(index) => match self.datums.get(*index) {
                Some(uqa_sql::plpgsql::PLpgSQLDatum::Var(variable))
                    if variable.name.eq_ignore_ascii_case("sqlstate") =>
                {
                    Ok(self.err_stack.last().map_or_else(
                        || self.values[*index].clone(),
                        |(state, _)| Value::Str(state.clone()),
                    ))
                }
                Some(uqa_sql::plpgsql::PLpgSQLDatum::Var(variable))
                    if variable.name.eq_ignore_ascii_case("sqlerrm") =>
                {
                    Ok(self.err_stack.last().map_or_else(
                        || self.values[*index].clone(),
                        |(_, message)| Value::Str(message.clone()),
                    ))
                }
                Some(
                    uqa_sql::plpgsql::PLpgSQLDatum::Var(_)
                    | uqa_sql::plpgsql::PLpgSQLDatum::Rec { .. },
                ) => Ok(self.values[*index].clone()),
                Some(uqa_sql::plpgsql::PLpgSQLDatum::Row { fields }) => Ok(Value::Record(
                    fields
                        .iter()
                        .map(|field| (field.name.clone(), self.values[field.varno].clone()))
                        .collect(),
                )),
                Some(_) => Err(SQLError::Internal(format!(
                    "PL/pgSQL retvarno {index} is not a returnable datum"
                ))),
                None => Err(SQLError::Internal(format!(
                    "PL/pgSQL RETURN references missing datum {index}"
                ))),
            },
        }
    }

    pub(super) fn append_query_rows(&mut self, result: &SQLResult) -> Result<(), SQLError> {
        let expected = if self.out_datums.is_empty() {
            1
        } else {
            self.out_datums.len()
        };
        if result.columns.len() != expected {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "structure of query does not match function result type".into(),
            });
        }
        for row_index in 0..result.rows.len() {
            let values = result_row_values(result, row_index).unwrap_or_default();
            self.set_rows.push(values);
        }
        Ok(())
    }
}
