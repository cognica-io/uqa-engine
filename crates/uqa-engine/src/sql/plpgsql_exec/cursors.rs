//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound PL/pgSQL cursor execution.

use super::{
    Interpreter, IntoTarget, PLpgSQLCursorArgument, PLpgSQLCursorState, PLpgSQLDatum,
    PLpgSQLRowField, SQLError, Value,
};

impl Interpreter<'_> {
    pub(super) fn exec_open_cursor(
        &mut self,
        cursor_index: usize,
        arguments: &[PLpgSQLCursorArgument],
    ) -> Result<(), SQLError> {
        let (cursor_name, query, argument_fields) = {
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
            (variable.name.clone(), definition.query.clone(), fields)
        };

        let portal_name = match self.values.get(cursor_index) {
            Some(Value::Str(name)) => name.clone(),
            Some(Value::Null) => {
                let name = format!("<unnamed portal {}>", self.next_cursor_id);
                self.next_cursor_id += 1;
                name
            }
            Some(value) => {
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: format!("cursor variable `{cursor_name}` contains {value:?}"),
                });
            }
            None => {
                return Err(SQLError::Internal(format!(
                    "PL/pgSQL OPEN references missing cursor datum {cursor_index}"
                )));
            }
        };
        if self.cursors.contains_key(&portal_name) {
            return Err(SQLError::Routine {
                sqlstate: "42P03".into(),
                message: format!("cursor \"{portal_name}\" already in use"),
            });
        }

        let values = self.evaluate_cursor_arguments(&cursor_name, &argument_fields, arguments)?;
        let saved_values = argument_fields
            .iter()
            .map(|field| self.values[field.varno].clone())
            .collect::<Vec<_>>();
        for (field, value) in argument_fields.iter().zip(values) {
            if let Err(error) = self.assign_datum(field.varno, value) {
                self.restore_cursor_arguments(&argument_fields, &saved_values);
                return Err(error);
            }
        }
        for field in &argument_fields {
            self.push_binding(&field.name, field.varno);
        }
        let result = self.exec_query(&query);
        for field in argument_fields.iter().rev() {
            self.pop_binding(&field.name);
        }
        self.restore_cursor_arguments(&argument_fields, &saved_values);
        let result = result?;

        self.values[cursor_index] = Value::Str(portal_name.clone());
        self.cursors.insert(
            portal_name,
            PLpgSQLCursorState {
                result,
                position: 0,
            },
        );
        Ok(())
    }

    pub(super) fn exec_fetch_cursor(
        &mut self,
        cursor_index: usize,
        target: &IntoTarget,
        direction: i64,
        count: i64,
    ) -> Result<(), SQLError> {
        if direction != 0 || count != 1 {
            return Err(SQLError::Unsupported(
                "PL/pgSQL FETCH directions other than NEXT".into(),
            ));
        }
        let portal_name = self.open_portal_name(cursor_index, "FETCH")?;
        let (columns, row) = {
            let state = self
                .cursors
                .get_mut(&portal_name)
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "34000".into(),
                    message: format!("cursor \"{portal_name}\" does not exist"),
                })?;
            let row = state.result.rows.get(state.position).cloned();
            if row.is_some() {
                state.position += 1;
            }
            (state.result.columns.clone(), row)
        };
        self.assign_into(target, &columns, row.as_ref())?;
        let found = row.is_some();
        self.last_row_count = i64::from(found);
        self.set_found(found);
        Ok(())
    }

    pub(super) fn exec_close_cursor(&mut self, cursor_index: usize) -> Result<(), SQLError> {
        let portal_name = self.open_portal_name(cursor_index, "CLOSE")?;
        if self.cursors.remove(&portal_name).is_none() {
            return Err(SQLError::Routine {
                sqlstate: "34000".into(),
                message: format!("cursor \"{portal_name}\" does not exist"),
            });
        }
        Ok(())
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
                sqlstate: "34000".into(),
                message: format!("cursor \"{cursor_name}\" does not exist"),
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
