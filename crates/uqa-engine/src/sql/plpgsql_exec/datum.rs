//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PL/pgSQL datum and `INTO` assignment semantics.

use super::{
    coerce_routine_value, row_value, BTreeMap, Interpreter, IntoTarget, PLpgSQLDatum, ResultRow,
    SQLError, Value,
};

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
                Value::Map(_) | Value::Null => {
                    self.values[idx] = value;
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
                    Value::Map(map) => {
                        let key = map
                            .keys()
                            .find(|k| k.eq_ignore_ascii_case(field))
                            .cloned()
                            .ok_or_else(|| SQLError::Routine {
                                sqlstate: "42703".into(),
                                message: format!(
                                    "record \"{parent_name}\" has no field \"{field}\""
                                ),
                            })?;
                        map.insert(key, value);
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
        row: Option<&ResultRow>,
    ) -> Result<(), SQLError> {
        match target {
            IntoTarget::Rec(dno) => {
                let value = match row {
                    Some(row) => {
                        let mut record = BTreeMap::new();
                        for column in columns {
                            record.insert(column.clone(), row_value(row, column));
                        }
                        Value::Map(record)
                    }
                    None => Value::Null,
                };
                self.values[*dno] = value;
                Ok(())
            }
            IntoTarget::Row(fields) => {
                for (idx, field) in fields.iter().enumerate() {
                    let value = match (row, columns.get(idx)) {
                        (Some(row), Some(column)) => row_value(row, column),
                        _ => Value::Null,
                    };
                    self.assign_datum(field.varno, value)?;
                }
                Ok(())
            }
        }
    }
}
