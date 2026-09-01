//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound PL/pgSQL cursor execution.

use super::{
    bind_statement, coerce_routine_value, compile, optimize_engine_plan, result_row_values,
    CursorDirection, Expr, FetchCursorStmt, Flow, Interpreter, IntoTarget, LoopSignal,
    PLpgSQLCursorArgument, PLpgSQLCursorCount, PLpgSQLCursorOpen, PLpgSQLDatum, PLpgSQLRowField,
    PLpgSQLStmt, SQLError, SQLParam, Statement, Value,
};
use uqa_planner::UnifiedPlan;

impl Interpreter<'_> {
    pub(super) fn exec_open_cursor(
        &mut self,
        cursor_index: usize,
        open: &PLpgSQLCursorOpen,
    ) -> Result<(), SQLError> {
        let cursor_name = self.cursor_variable_name(cursor_index, "OPEN")?.to_string();
        let portal_name = self.portal_name_for_open(cursor_index, &cursor_name)?;
        crate::sql::session_portal_worker::ensure_plpgsql_session_portal_available(
            self.engine,
            &portal_name,
        )?;
        let (query, params, scroll) = match open {
            PLpgSQLCursorOpen::Bound { arguments } => {
                let (query, fields, scroll) = self.bound_cursor_query(cursor_index)?;
                let query =
                    self.bind_bound_cursor_query(&cursor_name, query, &fields, arguments)?;
                (query, Vec::new(), scroll)
            }
            PLpgSQLCursorOpen::Static { query, scroll } => (
                bind_statement(query, &mut self.resolver())?,
                Vec::new(),
                *scroll,
            ),
            PLpgSQLCursorOpen::Dynamic {
                query,
                params,
                scroll,
            } => {
                let (text, params) = self.eval_dynamic_sql(query, params)?;
                (compile_cursor_statement(&text)?, params, *scroll)
            }
        };
        let plan = self.lower_cursor_plan(query)?;
        crate::sql::session_portal_worker::open_plpgsql_session_portal(
            self.engine,
            &params,
            &portal_name,
            scroll,
            &plan,
        )?;
        self.values[cursor_index] = Value::Str(portal_name);
        Ok(())
    }

    pub(super) fn exec_fetch_cursor(
        &mut self,
        cursor_index: usize,
        target: &IntoTarget,
        direction: CursorDirection,
        count: &PLpgSQLCursorCount,
    ) -> Result<(), SQLError> {
        let portal_name = self.open_portal_name(cursor_index, "FETCH")?;
        let count = self.eval_cursor_count(count)?;
        let result = self.engine.fetch_session_portal(&FetchCursorStmt {
            name: portal_name,
            direction,
            count,
            move_only: false,
        })?;
        let values = result.rows.first().map(|_| {
            (0..result.columns.len())
                .map(|column| result.value_at(0, column).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()
        });
        self.assign_into(target, &result.columns, values.as_deref())?;
        let found = !result.rows.is_empty();
        self.last_row_count = i64::from(found);
        self.set_found(found);
        Ok(())
    }

    pub(super) fn exec_move_cursor(
        &mut self,
        cursor_index: usize,
        direction: CursorDirection,
        count: &PLpgSQLCursorCount,
    ) -> Result<(), SQLError> {
        let portal_name = self.open_portal_name(cursor_index, "MOVE")?;
        let count = self.eval_cursor_count(count)?;
        let result = self.engine.fetch_session_portal(&FetchCursorStmt {
            name: portal_name,
            direction,
            count,
            move_only: true,
        })?;
        let moved = i64::try_from(result.affected_rows).map_err(|_| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor row count is out of range".into(),
        })?;
        self.last_row_count = moved;
        self.set_found(moved != 0);
        Ok(())
    }

    pub(super) fn exec_close_cursor(&mut self, cursor_index: usize) -> Result<(), SQLError> {
        let portal_name = self.open_portal_name(cursor_index, "CLOSE")?;
        self.engine.close_session_portal(&portal_name)
    }

    pub(super) fn exec_query_for(
        &mut self,
        label: Option<&str>,
        target: &IntoTarget,
        query: &Statement,
        body: &[PLpgSQLStmt],
    ) -> Result<Flow, SQLError> {
        let query = bind_statement(query, &mut self.resolver())?;
        let plan = self.lower_cursor_plan(query)?;
        let portal_name = self.open_internal_for_portal(&[], &plan)?;
        self.exec_pinned_for_portal(&portal_name, label, target, body, true)
    }

    pub(super) fn exec_dynamic_for(
        &mut self,
        label: Option<&str>,
        target: &IntoTarget,
        query: &Expr,
        params: &[Expr],
        body: &[PLpgSQLStmt],
    ) -> Result<Flow, SQLError> {
        let (text, params) = self.eval_dynamic_sql(query, params)?;
        let query = compile_cursor_statement(&text)?;
        let plan = self.lower_cursor_plan(query)?;
        let portal_name = self.open_internal_for_portal(&params, &plan)?;
        self.exec_pinned_for_portal(&portal_name, label, target, body, true)
    }

    pub(super) fn exec_cursor_for(
        &mut self,
        label: Option<&str>,
        target: usize,
        cursor: usize,
        arguments: &[PLpgSQLCursorArgument],
        body: &[PLpgSQLStmt],
    ) -> Result<Flow, SQLError> {
        let cursor_was_null = match self.values.get(cursor) {
            Some(value) => matches!(value, Value::Null),
            None => {
                return Err(SQLError::Internal(format!(
                    "PL/pgSQL bound-cursor FOR references missing datum {cursor}"
                )));
            }
        };
        self.exec_open_cursor(
            cursor,
            &PLpgSQLCursorOpen::Bound {
                arguments: arguments.to_vec(),
            },
        )?;
        let portal_name = self.open_portal_name(cursor, "FOR")?;
        let target = IntoTarget::Rec(target);
        let result = self.exec_pinned_for_portal(&portal_name, label, &target, body, false);
        if cursor_was_null {
            self.values[cursor] = Value::Null;
        }
        result
    }

    fn open_internal_for_portal(
        &self,
        params: &[SQLParam],
        plan: &UnifiedPlan,
    ) -> Result<String, SQLError> {
        let portal_name = self.engine.allocate_session_portal_name();
        crate::sql::session_portal_worker::open_plpgsql_session_portal(
            self.engine,
            params,
            &portal_name,
            Some(false),
            plan,
        )?;
        Ok(portal_name)
    }

    fn exec_pinned_for_portal(
        &mut self,
        portal_name: &str,
        label: Option<&str>,
        target: &IntoTarget,
        body: &[PLpgSQLStmt],
        prefetch: bool,
    ) -> Result<Flow, SQLError> {
        if let Err(error) = self.engine.pin_session_portal(portal_name) {
            let _ = self.engine.close_session_portal(portal_name);
            return Err(error);
        }
        let loop_result = self.exec_for_portal_rows(portal_name, label, target, body, prefetch);
        let unpin_result = self.engine.unpin_session_portal(portal_name);
        let close_result = match &unpin_result {
            Ok(()) => self.engine.close_session_portal(portal_name),
            Err(_) => Ok(()),
        };
        match loop_result {
            Err(error) => Err(error),
            Ok(_) if unpin_result.is_err() => Err(unpin_result.unwrap_err()),
            Ok(_) if close_result.is_err() => Err(close_result.unwrap_err()),
            Ok(flow) => Ok(flow),
        }
    }

    fn exec_for_portal_rows(
        &mut self,
        portal_name: &str,
        label: Option<&str>,
        target: &IntoTarget,
        body: &[PLpgSQLStmt],
        prefetch: bool,
    ) -> Result<Flow, SQLError> {
        let mut iterated = false;
        let mut outcome = Flow::Normal;
        let mut fetch_count = if prefetch { 10 } else { 1 };
        let mut initial_fetch = true;
        'batches: loop {
            let result = self.engine.fetch_session_portal(&FetchCursorStmt {
                name: portal_name.to_string(),
                direction: CursorDirection::Forward,
                count: fetch_count,
                move_only: false,
            })?;
            if result.rows.is_empty() {
                if initial_fetch {
                    self.assign_into(target, &result.columns, None)?;
                }
                break;
            }
            initial_fetch = false;
            iterated = true;
            for row_index in 0..result.rows.len() {
                let values = result_row_values(&result, row_index);
                self.assign_into(target, &result.columns, values.as_deref())?;
                match self.exec_loop_body(label, body)? {
                    LoopSignal::Continue => {}
                    LoopSignal::Break => break 'batches,
                    LoopSignal::Propagate(flow) => {
                        outcome = flow;
                        break 'batches;
                    }
                }
            }
            fetch_count = if prefetch { 50 } else { 1 };
        }
        self.set_found(iterated);
        Ok(outcome)
    }

    fn cursor_variable_name(&self, cursor_index: usize, operation: &str) -> Result<&str, SQLError> {
        match self.datums.get(cursor_index) {
            Some(PLpgSQLDatum::Var(variable)) => Ok(&variable.name),
            Some(_) => Err(SQLError::Internal(format!(
                "PL/pgSQL {operation} datum {cursor_index} is not a cursor variable"
            ))),
            None => Err(SQLError::Internal(format!(
                "PL/pgSQL {operation} references missing cursor datum {cursor_index}"
            ))),
        }
    }

    fn portal_name_for_open(
        &self,
        cursor_index: usize,
        cursor_name: &str,
    ) -> Result<String, SQLError> {
        match self.values.get(cursor_index) {
            Some(Value::Str(name)) => Ok(name.clone()),
            Some(Value::Null) => Ok(self.engine.allocate_session_portal_name()),
            Some(value) => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!("cursor variable `{cursor_name}` contains {value:?}"),
            }),
            None => Err(SQLError::Internal(format!(
                "PL/pgSQL OPEN references missing cursor datum {cursor_index}"
            ))),
        }
    }

    fn bound_cursor_query(
        &self,
        cursor_index: usize,
    ) -> Result<(Statement, Vec<PLpgSQLRowField>, Option<bool>), SQLError> {
        let Some(PLpgSQLDatum::Var(variable)) = self.datums.get(cursor_index) else {
            return Err(SQLError::Internal(format!(
                "PL/pgSQL OPEN references invalid cursor datum {cursor_index}"
            )));
        };
        let Some(definition) = &variable.cursor else {
            return Err(SQLError::Unsupported(
                "PL/pgSQL OPEN without FOR requires a bound cursor".into(),
            ));
        };
        let fields = match definition.argument_row {
            Some(row_index) => match self.datums.get(row_index) {
                Some(PLpgSQLDatum::Row { fields }) => fields.clone(),
                _ => {
                    return Err(SQLError::Internal(format!(
                        "PL/pgSQL cursor `{}` references invalid argument row {row_index}",
                        variable.name
                    )));
                }
            },
            None => Vec::new(),
        };
        Ok((definition.query.clone(), fields, definition.scroll))
    }

    fn bind_bound_cursor_query(
        &mut self,
        cursor_name: &str,
        query: Statement,
        fields: &[PLpgSQLRowField],
        arguments: &[PLpgSQLCursorArgument],
    ) -> Result<Statement, SQLError> {
        let values = self.evaluate_cursor_arguments(cursor_name, fields, arguments)?;
        let saved_values = fields
            .iter()
            .map(|field| self.values[field.varno].clone())
            .collect::<Vec<_>>();
        for (field, value) in fields.iter().zip(values) {
            if let Err(error) = self.assign_datum(field.varno, value) {
                self.restore_cursor_arguments(fields, &saved_values);
                return Err(error);
            }
        }
        if !fields.is_empty() {
            self.last_row_count = 1;
            self.set_found(true);
        }
        for field in fields {
            self.push_binding(&field.name, field.varno);
        }
        let query = bind_statement(&query, &mut self.resolver());
        for field in fields.iter().rev() {
            self.pop_binding(&field.name);
        }
        self.restore_cursor_arguments(fields, &saved_values);
        query
    }

    fn lower_cursor_plan(&self, statement: Statement) -> Result<UnifiedPlan, SQLError> {
        let plan = UnifiedPlan::lower_with(statement, &|name: &str| {
            self.engine.has_registered_aggregate_function(name)
        });
        optimize_engine_plan(self.engine, plan)
    }

    fn eval_cursor_count(&self, count: &PLpgSQLCursorCount) -> Result<i64, SQLError> {
        let expression = match count {
            PLpgSQLCursorCount::Constant(count) => return Ok(*count),
            PLpgSQLCursorCount::Expression(expression) => expression,
        };
        let value = self.eval_expr(expression)?;
        if matches!(value, Value::Null) {
            return Err(SQLError::Routine {
                sqlstate: "22004".into(),
                message: "relative or absolute cursor position is null".into(),
            });
        }
        match coerce_routine_value(self.engine, &value, "int4")? {
            Value::Int(count) => Ok(count),
            value => Err(SQLError::Internal(format!(
                "PL/pgSQL cursor count coercion returned {value:?}"
            ))),
        }
    }

    fn evaluate_cursor_arguments(
        &self,
        cursor_name: &str,
        fields: &[PLpgSQLRowField],
        arguments: &[PLpgSQLCursorArgument],
    ) -> Result<Vec<Value>, SQLError> {
        if fields.len() != arguments.len() {
            return Err(SQLError::Internal(format!(
                "PL/pgSQL cursor `{cursor_name}` expected {} arguments but received {}",
                fields.len(),
                arguments.len()
            )));
        }
        let mut values = vec![None; fields.len()];
        let mut next_positional = 0usize;
        for argument in arguments {
            let index = if let Some(name) = &argument.name {
                fields
                    .iter()
                    .position(|field| field.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "PL/pgSQL cursor `{cursor_name}` has no argument named `{name}`"
                        ))
                    })?
            } else {
                while next_positional < values.len() && values[next_positional].is_some() {
                    next_positional += 1;
                }
                next_positional
            };
            let Some(slot) = values.get_mut(index) else {
                return Err(SQLError::Internal(format!(
                    "PL/pgSQL cursor `{cursor_name}` received too many positional arguments"
                )));
            };
            if slot.is_some() {
                return Err(SQLError::Internal(format!(
                    "PL/pgSQL cursor `{cursor_name}` argument `{}` was specified more than once",
                    fields[index].name
                )));
            }
            *slot = Some(self.eval_expr(&argument.expr)?);
            if argument.name.is_none() {
                next_positional += 1;
            }
        }
        values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                value.ok_or_else(|| {
                    SQLError::Internal(format!(
                        "PL/pgSQL cursor `{cursor_name}` argument `{}` is missing",
                        fields[index].name
                    ))
                })
            })
            .collect()
    }

    fn restore_cursor_arguments(&mut self, fields: &[PLpgSQLRowField], saved: &[Value]) {
        for (field, value) in fields.iter().zip(saved) {
            self.values[field.varno] = value.clone();
        }
    }

    fn open_portal_name(&self, cursor_index: usize, operation: &str) -> Result<String, SQLError> {
        let cursor_name = self
            .datums
            .get(cursor_index)
            .and_then(PLpgSQLDatum::name)
            .unwrap_or("<unknown>");
        match self.values.get(cursor_index) {
            Some(Value::Str(name)) => Ok(name.clone()),
            Some(Value::Null) => Err(SQLError::Routine {
                sqlstate: "22004".into(),
                message: format!("cursor variable \"{cursor_name}\" is null"),
            }),
            Some(value) => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!("PL/pgSQL {operation} cursor contains {value:?}"),
            }),
            None => Err(SQLError::Internal(format!(
                "PL/pgSQL {operation} references missing cursor datum {cursor_index}"
            ))),
        }
    }
}

fn compile_cursor_statement(text: &str) -> Result<Statement, SQLError> {
    let mut statements = compile(text)?;
    if statements.len() != 1 {
        return Err(SQLError::Routine {
            sqlstate: "42P11".into(),
            message: "cannot open multi-query plan as cursor".into(),
        });
    }
    Ok(statements.remove(0))
}
