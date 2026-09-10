//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PL/pgSQL datum and `INTO` assignment semantics.

use super::{ColumnType, Interpreter, IntoTarget, PLpgSQLDatum, SQLError, Value};

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
        self.assign_datum_typed(idx, value, None, None)
    }

    pub(super) fn assign_datum_typed(
        &mut self,
        idx: usize,
        value: Value,
        source: Option<&ColumnType>,
        record_types: Option<Vec<Option<ColumnType>>>,
    ) -> Result<(), SQLError> {
        let target_type = self.datum_type(idx);
        match &self.datums[idx] {
            PLpgSQLDatum::Var(var) => {
                if var.constant {
                    return Err(SQLError::Routine {
                        sqlstate: "22005".into(),
                        message: format!("variable \"{}\" is declared CONSTANT", var.name),
                    });
                }
                let value = super::resolution::coerce_routine_value_from(
                    self.engine,
                    &value,
                    &var.type_name,
                    source,
                )?;
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
            PLpgSQLDatum::Rec { .. } => {
                if let Some(types) = record_types {
                    self.record_types.insert(idx, types);
                } else {
                    self.record_types.remove(&idx);
                }
                match value {
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
                }
            }
            PLpgSQLDatum::RecField { field, parent } => {
                let value = match target_type.as_ref() {
                    Some(target) => crate::sql::ddl::coerce_assignment_value(
                        self.engine,
                        value,
                        target,
                        source,
                    )?,
                    None => value,
                };
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

    /// Assign one `FOREACH` element to either a scalar/record datum or a
    /// comma-separated row target. Composite row fields are assigned by
    /// position, with missing attributes becoming NULL.
    pub(super) fn assign_foreach_target(
        &mut self,
        idx: usize,
        value: Value,
    ) -> Result<(), SQLError> {
        let datum = self.datums.get(idx).cloned().ok_or_else(|| {
            SQLError::Internal(format!("PL/pgSQL FOREACH references missing datum {idx}"))
        })?;
        let PLpgSQLDatum::Row { fields } = datum else {
            return self.assign_datum(idx, value);
        };
        let values = match value {
            Value::Record(fields) => fields
                .into_iter()
                .map(|(_, field_value)| field_value)
                .collect::<Vec<_>>(),
            Value::Row(values) => values,
            Value::Null => Vec::new(),
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "cannot assign non-composite value to a row variable".into(),
                });
            }
        };
        for (field_index, field) in fields.iter().enumerate() {
            self.assign_datum(
                field.varno,
                values.get(field_index).cloned().unwrap_or(Value::Null),
            )?;
        }
        Ok(())
    }

    /// Assign a query result row (or NULLs) to an INTO target.
    pub(super) fn assign_into(
        &mut self,
        target: &IntoTarget,
        columns: &[String],
        column_types: &[Option<ColumnType>],
        values: Option<&[Value]>,
    ) -> Result<(), SQLError> {
        match target {
            IntoTarget::Rec(dno) => {
                let value = Value::Record(
                    columns
                        .iter()
                        .enumerate()
                        .map(|(index, column)| {
                            (
                                column.clone(),
                                values
                                    .and_then(|values| values.get(index))
                                    .cloned()
                                    .unwrap_or(Value::Null),
                            )
                        })
                        .collect(),
                );
                self.values[*dno] = value;
                self.record_types.insert(*dno, column_types.to_vec());
                Ok(())
            }
            IntoTarget::Row(fields) => {
                for (idx, field) in fields.iter().enumerate() {
                    let value = values
                        .and_then(|values| values.get(idx))
                        .cloned()
                        .unwrap_or(Value::Null);
                    self.assign_datum_typed(
                        field.varno,
                        value,
                        column_types.get(idx).and_then(Option::as_ref),
                        None,
                    )?;
                }
                Ok(())
            }
        }
    }
}
