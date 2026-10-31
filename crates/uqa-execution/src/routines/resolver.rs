//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Variable resolution for bound PL/pgSQL expressions and statements.

use super::{DatumResolver, PLpgSQLDatum, ResolvedVariable, SQLError, Value, VariableResolver};
use uqa_sql::{ast::ColumnType, plpgsql::PLpgSQLVariableReference, SQLParam};

mod owned;
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

    pub(super) fn datum_type(&self, index: usize) -> Result<Option<ColumnType>, SQLError> {
        Ok(match &self.datums[index] {
            PLpgSQLDatum::Var(variable) => match self.descriptions {
                Some(descriptions) => descriptions.description.datum_type(
                    index,
                    &variable.type_name,
                    descriptions.environment,
                )?,
                None => self.types.catalog_column_type(&variable.type_name),
            }
            .filter(|ty| !matches!(ty, ColumnType::AnyArray | ColumnType::Record)),
            PLpgSQLDatum::RecField { parent, field } => self.record_field_type(*parent, field),
            PLpgSQLDatum::Rec { .. } => match &self.values[index] {
                Value::Datum(datum) => self
                    .types
                    .resolve_type_oid(datum.type_oid())
                    .map_err(SQLError::Internal)?,
                _ => None,
            },
            PLpgSQLDatum::Row { .. } => None,
        })
    }

    fn record_field_type(&self, parent: usize, field: &str) -> Option<ColumnType> {
        let descriptor = self.record_descriptors.get(&parent)?;
        descriptor
            .schema
            .column_type(descriptor.field_index(field)?)
            .cloned()
    }

    fn resolved_record_field(
        &self,
        parent: usize,
        field: &str,
    ) -> Result<Option<TypedVariable>, SQLError> {
        let descriptor = self.record_descriptors.get(&parent);
        let selected = match &self.values[parent] {
            Value::Record(fields) => fields
                .iter()
                .enumerate()
                .find_map(|(index, (name, value))| (name == field).then(|| (index, value.clone()))),
            Value::Datum(_) => {
                let descriptor = descriptor.ok_or_else(|| {
                    SQLError::Internal("physical record has no assignment descriptor".into())
                })?;
                match descriptor.field_index(field) {
                    Some(index) => descriptor
                        .read_field(&self.values[parent], field)?
                        .map(|value| (index, value)),
                    None => None,
                }
            }
            _ => None,
        };
        let Some((index, value)) = selected else {
            return Ok(None);
        };
        Ok(Some(TypedVariable {
            value,
            ty: descriptor
                .and_then(|descriptor| descriptor.schema.column_type(index))
                .cloned(),
            collation_oid: descriptor
                .and_then(|descriptor| descriptor.schema.column_collation(index)),
        }))
    }

    fn resolved_datum(&self, index: usize) -> Result<TypedVariable, SQLError> {
        if let PLpgSQLDatum::RecField { parent, field } = &self.datums[index] {
            return Ok(self
                .resolved_record_field(*parent, field)?
                .unwrap_or(TypedVariable {
                    value: Value::Null,
                    ty: None,
                    collation_oid: None,
                }));
        }
        Ok(TypedVariable {
            collation_oid: None,
            value: self.values[index].clone(),
            ty: self.datum_type(index)?,
        })
    }

    fn resolve_typed_name(&self, name: &str) -> Result<Option<TypedVariable>, SQLError> {
        if let Some((state, message)) = self.error {
            if name.eq_ignore_ascii_case("sqlstate") {
                return Ok(Some(TypedVariable {
                    collation_oid: None,
                    value: Value::Str(state.clone()),
                    ty: Some(ColumnType::Text),
                }));
            }
            if name.eq_ignore_ascii_case("sqlerrm") {
                return Ok(Some(TypedVariable {
                    collation_oid: None,
                    value: Value::Str(message.clone()),
                    ty: Some(ColumnType::Text),
                }));
            }
        }
        self.lookup(name)
            .map(|index| self.resolved_datum(index))
            .transpose()
    }

    fn resolve_typed_qualified(
        &self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<TypedVariable>, SQLError> {
        let Some(idx) = self.lookup(qualifier) else {
            return Ok(None);
        };
        match &self.datums[idx] {
            PLpgSQLDatum::Rec { name } => match &self.values[idx] {
                Value::Record(_) | Value::Datum(_) => {
                    match self.resolved_record_field(idx, column)? {
                        Some(value) => Ok(Some(value)),
                        None => Err(SQLError::Routine {
                            sqlstate: "42703".into(),
                            message: format!("record \"{name}\" has no field \"{column}\""),
                        }),
                    }
                }
                Value::Null => Err(super::diagnostics::unassigned_record(name)),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn resolve_typed_param(&self, index: usize) -> Result<Option<TypedVariable>, SQLError> {
        if index >= 1 && index <= self.param_count {
            Ok(Some(self.resolved_datum(index - 1)?))
        } else {
            Ok(None)
        }
    }
}

struct TypedVariable {
    value: Value,
    ty: Option<ColumnType>,
    collation_oid: Option<u32>,
}

impl TypedVariable {
    fn into_legacy(self) -> ResolvedVariable {
        ResolvedVariable {
            value: self.value,
            declared_type: self.ty.map(|ty| ty.catalog_name()),
            collation_oid: self.collation_oid,
        }
    }

    fn into_parameter(self) -> SQLParam {
        match self.ty {
            Some(ty) => SQLParam::typed_scalar(self.value, ty),
            None => SQLParam::scalar(self.value),
        }
    }
}

impl VariableResolver for DatumResolver<'_> {
    fn resolve_input_with_control(
        &mut self,
        reference: &PLpgSQLVariableReference,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<uqa_sql::plpgsql::PLpgSQLInput>>, SQLError> {
        self.resolve_owned_input(reference, control)
    }

    fn parameter_type(&self, name: &str) -> Option<ColumnType> {
        self.types.catalog_column_type(name)
    }

    fn resolve_parameter(
        &mut self,
        reference: &PLpgSQLVariableReference,
    ) -> Result<Option<SQLParam>, SQLError> {
        let value = match reference {
            PLpgSQLVariableReference::Name(name) => self.resolve_typed_name(name),
            PLpgSQLVariableReference::Qualified { qualifier, column } => {
                self.resolve_typed_qualified(qualifier, column)
            }
            PLpgSQLVariableReference::Parameter(index) => self.resolve_typed_param(*index),
        }?;
        Ok(value.map(TypedVariable::into_parameter))
    }

    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(self
            .resolve_typed_name(name)?
            .map(TypedVariable::into_legacy))
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(self
            .resolve_typed_qualified(qualifier, column)?
            .map(TypedVariable::into_legacy))
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(self
            .resolve_typed_param(index)?
            .map(TypedVariable::into_legacy))
    }
}
