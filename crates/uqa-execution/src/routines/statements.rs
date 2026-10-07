//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PL/pgSQL statement dispatch.

use super::{
    result_row_count, result_row_values, return_query_context_error, strict_into_check, Flow,
    Interpreter, LoopSignal, PLpgSQLStmt, SQLError, Value,
};

impl Interpreter<'_> {
    #[expect(clippy::too_many_lines, reason = "preserves PL/pgSQL transition order")]
    pub(super) fn exec_stmt(&mut self, stmt: &PLpgSQLStmt) -> Result<Flow, SQLError> {
        self.services.runtime.cancellation_token().check()?;
        match stmt {
            PLpgSQLStmt::Block(block) => self.exec_block(block),
            PLpgSQLStmt::Assign { target, expr } => self.exec_assignment(*target, expr),
            PLpgSQLStmt::If {
                cond,
                then_body,
                elsifs,
                else_body,
            } => self.exec_if(cond, then_body, elsifs, else_body.as_deref()),
            PLpgSQLStmt::Case {
                t_expr,
                t_varno,
                arms,
                else_body,
            } => self.exec_case(t_expr.as_ref(), *t_varno, arms, else_body.as_deref()),
            PLpgSQLStmt::Loop { label, body } => loop {
                match self.exec_loop_body(label.as_deref(), body)? {
                    LoopSignal::Continue => {}
                    LoopSignal::Break => return Ok(Flow::Normal),
                    LoopSignal::Propagate(flow) => return Ok(flow),
                }
            },
            PLpgSQLStmt::While { label, cond, body } => loop {
                if !self.eval_boolean(cond)?.unwrap_or(false) {
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
                source_sql,
                body,
            } => self.exec_query_for(label.as_deref(), target, query, source_sql, body),
            PLpgSQLStmt::ForDynamic {
                label,
                target,
                query,
                params,
                body,
            } => self.exec_dynamic_for(label.as_deref(), target, query, params, body),
            PLpgSQLStmt::ForCursor {
                label,
                target,
                cursor,
                arguments,
                body,
            } => {
                let name = self.datum_name(*target)?;
                self.push_binding(&name, *target);
                let result =
                    self.exec_cursor_for(label.as_deref(), *target, *cursor, arguments, body);
                self.pop_binding(&name);
                result
            }
            PLpgSQLStmt::ForeachArray {
                label,
                target,
                slice,
                expr,
                body,
            } => self.exec_foreach_array(label.as_deref(), *target, *slice, expr, body),
            PLpgSQLStmt::Exit {
                is_exit,
                label,
                cond,
            } => {
                if let Some(cond) = cond {
                    if !self.eval_boolean(cond)?.unwrap_or(false) {
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
                options,
            } => self.exec_raise(
                *level,
                condition.as_deref(),
                message.as_deref(),
                params,
                options,
            ),
            PLpgSQLStmt::Assert { condition, message } => {
                self.exec_assert(condition, message.as_ref())
            }
            PLpgSQLStmt::ExecSQL { stmt, into, strict } => {
                let prepared = self.prepare_statement(stmt)?;
                let result = self.execute_fragment(&prepared)?;
                self.consume_statement_result(prepared.is_call(), &result, into.as_ref(), *strict)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::DynExecute {
                query,
                params,
                into,
                strict,
            } => self.exec_dynamic_into(query, params, into.as_ref(), *strict),
            PLpgSQLStmt::Perform { query } => {
                let result = self.exec_query(query)?;
                let row_count = result_row_count(&result)?;
                self.last_row_count = row_count;
                self.set_found(row_count > 0);
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::OpenCursor { cursor, open } => {
                self.exec_open_cursor(*cursor, open)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::FetchCursor {
                cursor,
                target,
                direction,
                count,
            } => {
                self.exec_fetch_cursor(*cursor, target, *direction, count)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::MoveCursor {
                cursor,
                direction,
                count,
            } => {
                self.exec_move_cursor(*cursor, *direction, count)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::CloseCursor { cursor } => {
                self.exec_close_cursor(*cursor)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::Commit { chain } => self.exec_procedural_transaction(true, *chain),
            PLpgSQLStmt::Rollback { chain } => self.exec_procedural_transaction(false, *chain),
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

    fn exec_assignment(
        &mut self,
        target: usize,
        expr: &super::PLpgSQLExpression,
    ) -> Result<Flow, SQLError> {
        let record_types = self.record_expression_types(expr)?;
        let (value, source) = self.eval_expr_with_type(expr)?;
        self.assign_datum_typed(target, value, source.as_ref(), record_types)?;
        Ok(Flow::Normal)
    }

    fn exec_if(
        &mut self,
        cond: &super::PLpgSQLExpression,
        then_body: &[PLpgSQLStmt],
        elsifs: &[(super::PLpgSQLExpression, Vec<PLpgSQLStmt>)],
        else_body: Option<&[PLpgSQLStmt]>,
    ) -> Result<Flow, SQLError> {
        if self.eval_boolean(cond)?.unwrap_or(false) {
            return self.exec_stmts(then_body);
        }
        for (elsif_cond, body) in elsifs {
            if self.eval_boolean(elsif_cond)?.unwrap_or(false) {
                return self.exec_stmts(body);
            }
        }
        match else_body {
            Some(body) => self.exec_stmts(body),
            None => Ok(Flow::Normal),
        }
    }

    fn exec_case(
        &mut self,
        t_expr: Option<&super::PLpgSQLExpression>,
        t_varno: Option<usize>,
        arms: &[(super::PLpgSQLExpression, Vec<PLpgSQLStmt>)],
        else_body: Option<&[PLpgSQLStmt]>,
    ) -> Result<Flow, SQLError> {
        if let (Some(t_expr), Some(varno)) = (t_expr, t_varno) {
            let value = self.eval_expr(t_expr)?;
            self.values[varno] = value;
        }
        for (cond, body) in arms {
            if self.eval_boolean(cond)?.unwrap_or(false) {
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

    fn exec_dynamic_into(
        &mut self,
        query: &super::PLpgSQLExpression,
        params: &[super::PLpgSQLExpression],
        into: Option<&super::IntoTarget>,
        strict: bool,
    ) -> Result<Flow, SQLError> {
        let result = self.exec_dynamic(query, params)?;
        let row_count = result_row_count(&result)?;
        self.last_row_count = row_count;
        if let Some(target) = into {
            if strict {
                strict_into_check(row_count)?;
            }
            let values = result_row_values(&result, 0);
            self.assign_into(
                target,
                &result.columns,
                &result.column_types,
                values.as_deref(),
            )?;
        }
        // PostgreSQL: EXECUTE never changes FOUND.
        Ok(Flow::Normal)
    }
}
