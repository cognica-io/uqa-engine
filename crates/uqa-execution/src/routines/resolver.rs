//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Variable resolution for bound PL/pgSQL expressions and statements.

use super::{DatumResolver, PLpgSQLDatum, ResolvedVariable, SQLError, Value, VariableResolver};
use uqa_sql::ast::ColumnType;

#[cfg(test)]
mod tests;

impl DatumResolver<'_> {
    pub(super) fn parameter_number(
        &self,
        reference: &uqa_sql::plpgsql::PLpgSQLVariableReference,
    ) -> Option<usize> {
        use uqa_sql::plpgsql::PLpgSQLVariableReference;
        match reference {
            PLpgSQLVariableReference::Name(name) => self.lookup(name).map(|index| index + 1),
            PLpgSQLVariableReference::Parameter(index) => Some(*index),
            PLpgSQLVariableReference::Qualified { qualifier, column } => {
                let index = self.lookup(qualifier)?;
                self.datums.iter().position(|datum| matches!(datum,
                    PLpgSQLDatum::RecField { parent, field } if *parent == index && field == column
                )).map(|index| index + 1)
            }
        }
    }

    pub(super) fn lookup(&self, name: &str) -> Option<usize> {
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

    pub(super) fn datum_type(&self, index: usize) -> Option<ColumnType> {
        match &self.datums[index] {
            PLpgSQLDatum::Var(variable) => self
                .types
                .catalog_column_type(&variable.type_name)
                .filter(|ty| !matches!(ty, ColumnType::AnyArray | ColumnType::Record)),
            PLpgSQLDatum::RecField { parent, field } => self.record_field_type(*parent, field),
            PLpgSQLDatum::Rec { .. } | PLpgSQLDatum::Row { .. } => None,
        }
    }

    fn record_field_type(&self, parent: usize, field: &str) -> Option<ColumnType> {
        let (index, _) = self.record_field(parent, field)?;
        self.record_schemas
            .get(&parent)?
            .column_types()
            .get(index)?
            .clone()
    }

    fn record_field(&self, parent: usize, field: &str) -> Option<(usize, &Value)> {
        let Value::Record(fields) = &self.values[parent] else {
            return None;
        };
        fields
            .iter()
            .enumerate()
            .find_map(|(index, (name, value))| (name == field).then_some((index, value)))
    }

    fn resolved_record_field(&self, parent: usize, field: &str) -> Option<ResolvedVariable> {
        let (index, value) = self.record_field(parent, field)?;
        let schema = self.record_schemas.get(&parent);
        Some(ResolvedVariable {
            value: value.clone(),
            declared_type: schema
                .and_then(|schema| schema.column_type(index))
                .map(ColumnType::catalog_name),
            collation_oid: schema.and_then(|schema| schema.column_collation(index)),
        })
    }

    fn resolved_datum(&self, index: usize) -> ResolvedVariable {
        if let PLpgSQLDatum::RecField { parent, field } = &self.datums[index] {
            return self
                .resolved_record_field(*parent, field)
                .unwrap_or(ResolvedVariable {
                    value: Value::Null,
                    declared_type: None,
                    collation_oid: None,
                });
        }
        ResolvedVariable {
            collation_oid: None,
            value: self.values[index].clone(),
            declared_type: self.datum_type(index).map(|ty| ty.catalog_name()),
        }
    }
}

impl VariableResolver for DatumResolver<'_> {
    fn parameter_type(&self, name: &str) -> Option<ColumnType> {
        self.types.catalog_column_type(name)
    }
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if let Some((state, message)) = self.error {
            if name.eq_ignore_ascii_case("sqlstate") {
                return Ok(Some(ResolvedVariable {
                    collation_oid: None,
                    value: Value::Str(state.clone()),
                    declared_type: Some("text".into()),
                }));
            }
            if name.eq_ignore_ascii_case("sqlerrm") {
                return Ok(Some(ResolvedVariable {
                    collation_oid: None,
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
                Value::Record(_) => match self.resolved_record_field(idx, column) {
                    Some(value) => Ok(Some(value)),
                    None => Err(SQLError::Routine {
                        sqlstate: "42703".into(),
                        message: format!("record \"{name}\" has no field \"{column}\""),
                    }),
                },
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
