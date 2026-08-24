//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Variable resolution for bound PL/pgSQL expressions and statements.

use super::{DatumResolver, PLpgSQLDatum, ResolvedVariable, SQLError, Value, VariableResolver};
use uqa_sql::ast::ColumnType;

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

    fn resolved_datum(&self, index: usize) -> ResolvedVariable {
        let value = self.values[index].clone();
        let declared_type = match &self.datums[index] {
            PLpgSQLDatum::Var(variable) => {
                crate::sql::resolve_catalog_column_type(self.engine, &variable.type_name)
                    .filter(|ty| !matches!(ty, ColumnType::AnyArray | ColumnType::Record))
                    .map(|_| variable.type_name.clone())
            }
            PLpgSQLDatum::Rec { .. } | PLpgSQLDatum::RecField { .. } | PLpgSQLDatum::Row { .. } => {
                None
            }
        };
        ResolvedVariable {
            value,
            declared_type,
        }
    }
}

impl VariableResolver for DatumResolver<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if let Some((state, message)) = self.error {
            if name.eq_ignore_ascii_case("sqlstate") {
                return Ok(Some(ResolvedVariable {
                    value: Value::Str(state.clone()),
                    declared_type: Some("text".into()),
                }));
            }
            if name.eq_ignore_ascii_case("sqlerrm") {
                return Ok(Some(ResolvedVariable {
                    value: Value::Str(message.clone()),
                    declared_type: Some("text".into()),
                }));
            }
        }
        Ok(self.lookup(name).map(|index| self.resolved_datum(index)))
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        let Some(idx) = self.lookup(qualifier) else {
            return Ok(None);
        };
        match &self.datums[idx] {
            PLpgSQLDatum::Rec { name } => match &self.values[idx] {
                Value::Record(fields) => {
                    let value = fields
                        .iter()
                        .find(|(name, _)| name == column)
                        .map(|(_, value)| value.clone());
                    match value {
                        Some(value) => Ok(Some(ResolvedVariable::untyped(value))),
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

    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        if index >= 1 && index <= self.param_count {
            Ok(Some(self.resolved_datum(index - 1)))
        } else {
            Ok(None)
        }
    }
}
