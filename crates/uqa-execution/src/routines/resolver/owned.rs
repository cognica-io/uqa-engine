//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation copies selected activation values and descriptors directly into its original request owner.

use super::{ColumnType, DatumResolver, PLpgSQLDatum, PLpgSQLVariableReference, SQLError, Value};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::plpgsql::PLpgSQLInput;

impl DatumResolver<'_> {
    fn lookup_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<usize>, SQLError> {
        control.check()?;
        if let Some(stack) = self.bindings.get(name) {
            return Ok(stack.last().copied());
        }
        if !name.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Ok(None);
        }
        for (candidate, stack) in self.bindings {
            control.check()?;
            if !candidate.bytes().any(|byte| byte.is_ascii_uppercase())
                && candidate.eq_ignore_ascii_case(name)
            {
                return Ok(stack.last().copied());
            }
        }
        Ok(None)
    }

    pub(super) fn resolve_owned_input(
        &self,
        reference: &PLpgSQLVariableReference,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<PLpgSQLInput>>, SQLError> {
        control.check()?;
        match reference {
            PLpgSQLVariableReference::Name(name) => {
                if let Some((state, message)) = self.error {
                    let text = if name.eq_ignore_ascii_case("sqlstate") {
                        Some(state)
                    } else if name.eq_ignore_ascii_case("sqlerrm") {
                        Some(message)
                    } else {
                        None
                    };
                    if let Some(text) = text {
                        let (text, memory) = control.copy_text(text)?.into_parts();
                        return Ok(Some(PLpgSQLInput::from_parts(
                            control.finish(Value::Str(text), memory)?,
                            Some(ColumnType::Text.clone_with_control(control)?),
                            None,
                            control,
                        )?));
                    }
                }
                self.lookup_with_control(name, control)?
                    .map(|index| self.datum_input(index, control))
                    .transpose()
            }
            PLpgSQLVariableReference::Parameter(index) => {
                if *index >= 1 && *index <= self.param_count {
                    self.datum_input(index - 1, control).map(Some)
                } else {
                    Ok(None)
                }
            }
            PLpgSQLVariableReference::Qualified { qualifier, column } => {
                let Some(index) = self.lookup_with_control(qualifier, control)? else {
                    return Ok(None);
                };
                let PLpgSQLDatum::Rec { name } = &self.datums[index] else {
                    return Ok(None);
                };
                match &self.values[index] {
                    Value::Record(_) | Value::Datum(_) => self
                        .field_input(index, column, control)?
                        .map(Some)
                        .ok_or_else(|| SQLError::Routine {
                            sqlstate: "42703".into(),
                            message: format!("record \"{name}\" has no field \"{column}\""),
                        }),
                    Value::Null => Err(crate::routines::diagnostics::unassigned_record(name)),
                    _ => Ok(None),
                }
            }
        }
    }

    fn datum_input(
        &self,
        index: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<PLpgSQLInput>, SQLError> {
        if let PLpgSQLDatum::RecField { parent, field } = &self.datums[index] {
            return match self.field_input(*parent, field, control)? {
                Some(value) => Ok(value),
                None => {
                    PLpgSQLInput::from_parts(control.copy_value(&Value::Null)?, None, None, control)
                }
            };
        }
        // Preserve value-before-type lookup, including lazy record decoding and declaration callbacks.
        let value = control.copy_value(&self.values[index])?;
        let ty = match &self.datums[index] {
            PLpgSQLDatum::Var(variable) => match self.descriptions {
                Some(descriptions) => descriptions.description.datum_type_with_control(
                    index,
                    &variable.type_name,
                    descriptions.environment,
                    control,
                )?,
                None => self
                    .types
                    .catalog_column_type_with_control(&variable.type_name, control)?,
            }
            .filter(|ty| !matches!(&**ty, ColumnType::AnyArray | ColumnType::Record)),
            PLpgSQLDatum::Rec { .. } => match &self.values[index] {
                Value::Datum(datum) => self
                    .types
                    .resolve_type_oid_with_control(datum.type_oid(), control)?,
                _ => None,
            },
            PLpgSQLDatum::Row { .. } => None,
            PLpgSQLDatum::RecField { .. } => unreachable!("record field selected above"),
        };
        PLpgSQLInput::from_parts(value, ty, None, control)
    }

    fn field_input(
        &self,
        parent: usize,
        field: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<PLpgSQLInput>>, SQLError> {
        let descriptor = self.record_descriptors.get(&parent);
        let selected = match &self.values[parent] {
            Value::Record(fields) => {
                let mut selected = None;
                for (index, (name, value)) in fields.iter().enumerate() {
                    control.check()?;
                    if name == field {
                        selected = Some((index, control.copy_value(value)?));
                        break;
                    }
                }
                selected
            }
            Value::Datum(datum) => {
                let descriptor = descriptor.ok_or_else(|| {
                    SQLError::Internal("physical record has no assignment descriptor".into())
                })?;
                descriptor
                    .field_index(field)
                    .map(|index| {
                        uqa_sql::expr::datums::read_composite_field_with_control(
                            datum,
                            field,
                            Some(descriptor),
                            control,
                        )
                        .map(|value| (index, value))
                    })
                    .transpose()?
            }
            _ => None,
        };
        let Some((index, value)) = selected else {
            return Ok(None);
        };
        let ty = descriptor
            .and_then(|descriptor| descriptor.schema.column_type(index))
            .map(|ty| ty.clone_with_control(control))
            .transpose()?;
        let collation = descriptor.and_then(|descriptor| descriptor.schema.column_collation(index));
        PLpgSQLInput::from_parts(value, ty, collation, control).map(Some)
    }
}
