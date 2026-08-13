//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PL/pgSQL statement dispatch.

use super::{
    result_row_count, return_query_context_error, strict_into_check, truthy, Flow, Interpreter,
    LoopSignal, PLpgSQLStmt, SQLError, Value,
};

impl Interpreter<'_> {
    pub(super) fn exec_stmt(&mut self, stmt: &PLpgSQLStmt) -> Result<Flow, SQLError> {
        self.engine.cancellation_token().check()?;
        match stmt {
            PLpgSQLStmt::Block(block) => self.exec_block(block),
            PLpgSQLStmt::Assign { target, expr } => {
                let value = self.eval_expr(expr)?;
                self.assign_datum(*target, value)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::If {
                cond,
                then_body,
                elsifs,
                else_body,
            } => {
                if truthy(&self.eval_expr(cond)?) {
                    return self.exec_stmts(then_body);
                }
                for (elsif_cond, body) in elsifs {
                    if truthy(&self.eval_expr(elsif_cond)?) {
                        return self.exec_stmts(body);
                    }
                }
                match else_body {
                    Some(body) => self.exec_stmts(body),
                    None => Ok(Flow::Normal),
                }
            }
            PLpgSQLStmt::Case {
                t_expr,
                t_varno,
                arms,
                else_body,
            } => {
                if let (Some(t_expr), Some(varno)) = (t_expr, t_varno) {
                    let value = self.eval_expr(t_expr)?;
                    self.values[*varno] = value;
                }
                for (cond, body) in arms {
                    if truthy(&self.eval_expr(cond)?) {
                        return self.exec_stmts(body);
                    }
                }
                match else_body {
                    Some(body) => self.exec_stmts(body),
                    None => Err(SQLError::Routine {
                        sqlstate: "20000".into(),
                        message: "case not found".into(),
                    }),
                }
            }
            PLpgSQLStmt::Loop { label, body } => loop {
                match self.exec_loop_body(label.as_deref(), body)? {
                    LoopSignal::Continue => {}
                    LoopSignal::Break => return Ok(Flow::Normal),
                    LoopSignal::Propagate(flow) => return Ok(flow),
                }
            },
            PLpgSQLStmt::While { label, cond, body } => loop {
                if !truthy(&self.eval_expr(cond)?) {
                    return Ok(Flow::Normal);
                }
                match self.exec_loop_body(label.as_deref(), body)? {
                    LoopSignal::Continue => {}
                    LoopSignal::Break => return Ok(Flow::Normal),
                    LoopSignal::Propagate(flow) => return Ok(flow),
                }
            },
            PLpgSQLStmt::ForI {
                label,
                var,
                lower,
                upper,
                step,
                reverse,
                body,
            } => {
                let name = self.datum_name(*var)?;
                self.push_binding(&name, *var);
                let result = self.exec_fori(
                    label.as_deref(),
                    *var,
                    lower,
                    upper,
                    step.as_ref(),
                    *reverse,
                    body,
                );
                self.pop_binding(&name);
                result
            }
            PLpgSQLStmt::ForQuery {
                label,
                target,
                query,
                body,
            } => {
                let result = self.exec_query(query)?;
                let mut iterated = false;
                let mut outcome = Flow::Normal;
                for row in &result.rows {
                    iterated = true;
                    self.assign_into(target, &result.columns, Some(row))?;
                    match self.exec_loop_body(label.as_deref(), body)? {
                        LoopSignal::Continue => {}
                        LoopSignal::Break => break,
                        LoopSignal::Propagate(flow) => {
                            outcome = flow;
                            break;
                        }
                    }
                }
                self.set_found(iterated);
                Ok(outcome)
            }
            PLpgSQLStmt::Exit {
                is_exit,
                label,
                cond,
            } => {
                if let Some(cond) = cond {
                    if !truthy(&self.eval_expr(cond)?) {
                        return Ok(Flow::Normal);
                    }
                }
                if *is_exit {
                    Ok(Flow::Exit(label.clone()))
                } else {
                    Ok(Flow::Continue(label.clone()))
                }
            }
            PLpgSQLStmt::Return { value } => self.exec_return(value.as_ref()),
            PLpgSQLStmt::ReturnNext { value } => self.exec_return_next(value.as_ref()),
            PLpgSQLStmt::ReturnQuery { query } => {
                if !self.is_set {
                    return Err(return_query_context_error());
                }
                let result = self.exec_query(query)?;
                self.append_query_rows(&result)?;
                // PostgreSQL sets ROW_COUNT (but not FOUND) here.
                self.last_row_count = result_row_count(&result)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::ReturnQueryExecute { query, params } => {
                if !self.is_set {
                    return Err(return_query_context_error());
                }
                let result = self.exec_dynamic(query, params)?;
                self.append_query_rows(&result)?;
                self.last_row_count = result_row_count(&result)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::Raise {
                level,
                condition,
                message,
                params,
            } => self.exec_raise(*level, condition.as_deref(), message.as_deref(), params),
            PLpgSQLStmt::ExecSQL { stmt, into, strict } => {
                let result = self.exec_query(stmt)?;
                self.consume_statement_result(stmt, &result, into.as_ref(), *strict)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::DynExecute {
                query,
                params,
                into,
                strict,
            } => {
                let result = self.exec_dynamic(query, params)?;
                let row_count = result_row_count(&result)?;
                self.last_row_count = row_count;
                if let Some(target) = into {
                    if *strict {
                        strict_into_check(row_count)?;
                    }
                    self.assign_into(target, &result.columns, result.rows.first())?;
                }
                // PostgreSQL: EXECUTE never changes FOUND.
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::Perform { query } => {
                let result = self.exec_query(query)?;
                let row_count = result_row_count(&result)?;
                self.last_row_count = row_count;
                self.set_found(row_count > 0);
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::OpenCursor { cursor, arguments } => {
                self.exec_open_cursor(*cursor, arguments)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::FetchCursor {
                cursor,
                target,
                direction,
                count,
            } => {
                self.exec_fetch_cursor(*cursor, target, *direction, *count)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::CloseCursor { cursor } => {
                self.exec_close_cursor(*cursor)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::GetDiagnostics { items } => {
                for (kind, target) in items {
                    match kind.as_str() {
                        "ROW_COUNT" => {
                            let count = Value::Int(self.last_row_count);
                            self.assign_datum(*target, count)?;
                        }
                        other => {
                            return Err(SQLError::Unsupported(format!("GET DIAGNOSTICS {other}")));
                        }
                    }
                }
                Ok(Flow::Normal)
            }
        }
    }
}
