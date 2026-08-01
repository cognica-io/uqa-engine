//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Variable resolution for bound PL/pgSQL expressions and statements.

use super::{DatumResolver, PLpgSQLDatum, SQLError, Value, VariableResolver};

impl DatumResolver<'_> {
    fn lookup(&self, name: &str) -> Option<usize> {
        if let Some(stack) = self.bindings.get(name) {
            return stack.last().copied();
        }
        let lower = name.to_ascii_lowercase();
        if lower != name {
            if let Some(stack) = self.bindings.get(&lower) {
                return stack.last().copied();
            }
        }
        None
    }
}

impl VariableResolver for DatumResolver<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<Value>, SQLError> {
        if let Some((state, message)) = self.error {
            if name.eq_ignore_ascii_case("sqlstate") {
                return Ok(Some(Value::Str(state.clone())));
            }
            if name.eq_ignore_ascii_case("sqlerrm") {
                return Ok(Some(Value::Str(message.clone())));
            }
        }
        Ok(self.lookup(name).map(|idx| self.values[idx].clone()))
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Value>, SQLError> {
        let Some(idx) = self.lookup(qualifier) else {
            return Ok(None);
        };
        match &self.datums[idx] {
            PLpgSQLDatum::Rec { name } => match &self.values[idx] {
                Value::Map(map) => {
                    let value = map
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(column))
                        .map(|(_, value)| value.clone());
                    match value {
                        Some(value) => Ok(Some(value)),
                        None => Err(SQLError::Routine {
                            sqlstate: "42703".into(),
                            message: format!("record \"{name}\" has no field \"{column}\""),
                        }),
                    }
                }
                Value::Null => Err(SQLError::Routine {
                    sqlstate: "55000".into(),
                    message: format!("record \"{name}\" is not assigned yet"),
                }),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<Value>, SQLError> {
        if index >= 1 && index <= self.param_count {
            Ok(Some(self.values[index - 1].clone()))
        } else {
            Ok(None)
        }
    }
}
