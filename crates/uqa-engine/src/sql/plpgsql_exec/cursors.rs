//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound PL/pgSQL cursor execution.

use super::{
    bind_statement, coerce_routine_value, compile, optimize_engine_plan, CursorDirection,
    FetchCursorStmt, Interpreter, IntoTarget, PLpgSQLCursorArgument, PLpgSQLCursorCount,
    PLpgSQLCursorOpen, PLpgSQLDatum, PLpgSQLRowField, SQLError, Statement, Value,
};
use uqa_planner::{QueryPlan, UnifiedPlan};

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
        let query = self.lower_cursor_query(query)?;
        crate::sql::session_portal_worker::open_plpgsql_session_portal(
            self.engine,
            &params,
            &portal_name,
            scroll,
            &query,
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

    fn lower_cursor_query(&self, statement: Statement) -> Result<QueryPlan, SQLError> {
        let plan = UnifiedPlan::lower_with(statement, &|name: &str| {
            self.engine.has_registered_aggregate_function(name)
        });
        match optimize_engine_plan(self.engine, plan)? {
            UnifiedPlan::Query(query) => Ok(*query),
            UnifiedPlan::Command(command) => Err(SQLError::Routine {
                sqlstate: "42P11".into(),
                message: format!(
                    "cannot open {} query as cursor",
                    command.name().to_ascii_uppercase()
                ),
            }),
        }
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
