//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected record observation retains one descriptor and leaves unreached payloads unread.

use super::{SQLValueCatalog, ValueCompositeCatalog};
use crate::{expr::composites::CompositeTypeDescriptor, SQLError, SQLParam};
use std::sync::Arc;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

/// A record selected by its declared SQL type. Physical values use the tuple's actual type, while anonymous rows keep their producer's field identities. This short-lived view borrows the invoking catalog scope and invokes no input callbacks.
pub struct SQLRecordView<'a> {
    value: &'a Value,
    descriptor: Option<Arc<CompositeTypeDescriptor>>,
}

impl<'a> SQLRecordView<'a> {
    pub fn new(
        value: &'a Value,
        declared: Option<&crate::ColumnType>,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut declared = declared;
        while let Some(crate::ColumnType::Domain { base, .. }) = declared {
            declared = Some(base);
        }
        let declared_oid = if let Some(crate::ColumnType::Composite(reference)) = declared {
            Some(reference.oid)
        } else {
            None
        };
        let oid = match value {
            Value::Datum(datum) => Some(crate::expr::datums::record_type_oid(datum, control)?),
            Value::Record(record) => record.type_oid().or(declared_oid),
            Value::Row(_) => declared_oid,
            _ => {
                return Err(SQLError::TypeMismatch(
                    "record observation requires a record value".into(),
                ));
            }
        };
        let descriptor = oid
            .map(|oid| {
                crate::expr::composites::descriptor(Some(&ValueCompositeCatalog(catalog)), oid)
            })
            .transpose()?;
        Ok(Self { value, descriptor })
    }

    #[must_use]
    pub fn descriptor(&self) -> Option<&CompositeTypeDescriptor> {
        self.descriptor.as_deref()
    }

    /// Retain the selected catalog layout for a procedural record's field binding.
    #[must_use]
    pub fn into_descriptor(self) -> Option<Arc<CompositeTypeDescriptor>> {
        self.descriptor
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.descriptor.as_ref().map_or_else(
            || match self.value {
                Value::Record(record) => record.len(),
                Value::Row(row) => row.len(),
                _ => unreachable!("physical record has a descriptor"),
            },
            |descriptor| descriptor.attributes.len(),
        )
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Observe only one live, zero-based field. Its type and lazy value come from the same descriptor. NULL keeps the selected type, and physical addressing never invokes a type's input function.
    pub fn field_with_control(
        &self,
        index: usize,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<SQLParam>>, SQLError> {
        control.check()?;
        if index >= self.len() {
            return Ok(None);
        }
        let attribute = self
            .descriptor
            .as_ref()
            .map(|descriptor| &descriptor.attributes[index]);
        let ty = if let Some(attribute) = attribute {
            Some(attribute.ty.clone_with_control(control)?)
        } else if let Value::Row(row) = self.value {
            row.field_types()
                .map(|fields| fields[index])
                .filter(|field| field.oid != 705)
                .map(|field| {
                    super::types::field_type_with_control(
                        field.oid,
                        field.type_modifier,
                        catalog,
                        control,
                    )
                })
                .transpose()?
        } else {
            None
        };
        let value = match self.value {
            Value::Datum(datum) => crate::expr::datums::read_composite_field_in_descriptor(
                datum,
                &attribute.expect("physical record attribute").name,
                self.descriptor
                    .as_ref()
                    .expect("physical record descriptor"),
                control,
            )?,
            Value::Record(record) => {
                let value = attribute.map_or_else(
                    || &record.values()[index],
                    |attribute| {
                        record
                            .iter()
                            .find(|(name, _)| name.as_str() == attribute.name)
                            .map_or(&Value::Null, |(_, value)| value)
                    },
                );
                control.copy_value(value)?
            }
            Value::Row(row) => control.copy_value(&row[index])?,
            _ => unreachable!("selected record"),
        };
        let (value, memory) = value.into_parts();
        let parameter = if let Some(ty) = ty {
            let (ty, type_memory) = ty.into_parts();
            control.finish(
                SQLParam::TypedScalar { value, ty },
                control.combine(memory, type_memory),
            )?
        } else {
            control.finish(SQLParam::Scalar(value), memory)?
        };
        Ok(Some(parameter))
    }
}

#[cfg(test)]
mod tests;
