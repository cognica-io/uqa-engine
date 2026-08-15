//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PL/pgSQL datum and `INTO` assignment semantics.

use super::{coerce_routine_value, Interpreter, IntoTarget, PLpgSQLDatum, SQLError, Value};

impl Interpreter<'_> {
    pub(super) fn datum_name(&self, idx: usize) -> Result<String, SQLError> {
        let datum = self.datums.get(idx).ok_or_else(|| {
            SQLError::Internal(format!("PL/pgSQL references missing datum {idx}"))
        })?;
        datum.name().map(ToString::to_string).ok_or_else(|| {
            SQLError::Internal(format!(
                "PL/pgSQL datum {idx} does not have a bindable name"
            ))
        })
    }

    /// Store into a datum applying CONSTANT / type / NOT NULL rules.
    pub(super) fn assign_datum(&mut self, idx: usize, value: Value) -> Result<(), SQLError> {
        match &self.datums[idx] {
            PLpgSQLDatum::Var(var) => {
                if var.constant {
                    return Err(SQLError::Routine {
                        sqlstate: "22005".into(),
                        message: format!("variable \"{}\" is declared CONSTANT", var.name),
                    });
                }
                let value = coerce_routine_value(&value, &var.type_name)?;
                if var.not_null && matches!(value, Value::Null) {
                    return Err(SQLError::Routine {
                        sqlstate: "22004".into(),
                        message: format!(
                            "null value cannot be assigned to variable \"{}\" declared NOT NULL",
                            var.name
                        ),
                    });
                }
                self.values[idx] = value;
                Ok(())
            }
            PLpgSQLDatum::Rec { .. } => match value {
                Value::Record(_) | Value::Null => {
                    self.values[idx] = value;
                    Ok(())
                }
                Value::Row(values) => {
                    self.values[idx] = Value::Record(
                        values
                            .into_iter()
                            .enumerate()
                            .map(|(index, value)| (format!("f{}", index + 1), value))
                            .collect(),
                    );
                    Ok(())
                }
                _ => Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "cannot assign non-composite value to a record variable".into(),
                }),
            },
            PLpgSQLDatum::RecField { field, parent } => {
                let parent_name = self.datum_name(*parent)?;
                match &mut self.values[*parent] {
                    Value::Record(fields) => {
                        let field_value = fields
                            .iter_mut()
                            .find(|(name, _)| name == field)
                            .map(|(_, value)| value)
                            .ok_or_else(|| SQLError::Routine {
                                sqlstate: "42703".into(),
                                message: format!(
                                    "record \"{parent_name}\" has no field \"{field}\""
                                ),
                            })?;
                        *field_value = value;
                        Ok(())
                    }
                    _ => Err(SQLError::Routine {
                        sqlstate: "55000".into(),
                        message: format!("record \"{parent_name}\" is not assigned yet"),
                    }),
                }
            }
            PLpgSQLDatum::Row { .. } => Err(SQLError::Internal(
                "direct assignment to a row datum".into(),
            )),
        }
    }

    /// Assign a query result row (or NULLs) to an INTO target.
    pub(super) fn assign_into(
        &mut self,
        target: &IntoTarget,
        columns: &[String],
        values: Option<&[Value]>,
    ) -> Result<(), SQLError> {
        match target {
            IntoTarget::Rec(dno) => {
                let value = match values {
                    Some(values) => Value::Record(
                        columns
                            .iter()
                            .enumerate()
                            .map(|(index, column)| {
                                (
                                    column.clone(),
                                    values.get(index).cloned().unwrap_or(Value::Null),
                                )
                            })
                            .collect(),
                    ),
                    None => Value::Null,
                };
                self.values[*dno] = value;
                Ok(())
            }
            IntoTarget::Row(fields) => {
                for (idx, field) in fields.iter().enumerate() {
                    let value = values
                        .and_then(|values| values.get(idx))
                        .cloned()
                        .unwrap_or(Value::Null);
                    self.assign_datum(field.varno, value)?;
                }
                Ok(())
            }
        }
    }
}
