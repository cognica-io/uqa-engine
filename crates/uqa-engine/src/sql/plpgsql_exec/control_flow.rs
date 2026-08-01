//! Loop and return control-flow execution.

use super::{
    best_effort_cast, row_value, to_i64_value, Expr, Flow, FunctionReturns, Interpreter,
    LoopSignal, PLpgSQLStmt, SQLError, SQLResult, Value,
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

    #[allow(clippy::too_many_arguments)]
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

    pub(super) fn exec_return(&mut self, expr: Option<&Expr>) -> Result<Flow, SQLError> {
        if let Some(expr) = expr {
            let context = if self.def.is_procedure {
                Some("RETURN cannot have a parameter in a procedure")
            } else if self.is_set {
                Some("RETURN cannot have a parameter in function returning set")
            } else if !self.out_datums.is_empty() {
                Some("RETURN cannot have a parameter in function with OUT parameters")
            } else if matches!(
                &self.def.returns,
                FunctionReturns::Scalar { type_name } if type_name == "void"
            ) {
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
            let value = self.eval_expr(expr)?;
            self.ret = match &self.def.returns {
                FunctionReturns::Scalar { type_name } => best_effort_cast(&value, type_name)?,
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

    pub(super) fn exec_return_next(&mut self, expr: Option<&Expr>) -> Result<Flow, SQLError> {
        if !self.is_set {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "cannot use RETURN NEXT in a non-SETOF function".into(),
            });
        }
        if !self.out_datums.is_empty() {
            if expr.is_some() {
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
        let Some(expr) = expr else {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "RETURN NEXT must have a parameter".into(),
            });
        };
        let value = self.eval_expr(expr)?;
        let value = match &self.def.returns {
            FunctionReturns::SetOf { type_name } => best_effort_cast(&value, type_name)?,
            _ => value,
        };
        self.set_rows.push(vec![value]);
        Ok(Flow::Normal)
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
        for row in &result.rows {
            let values = result
                .columns
                .iter()
                .map(|column| row_value(row, column))
                .collect();
            self.set_rows.push(values);
        }
        Ok(())
    }
}
