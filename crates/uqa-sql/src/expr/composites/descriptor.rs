//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain a descriptor generation under the consumer's memory allowance.

use super::{CompositeAttribute, CompositeTypeDescriptor};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueRetentionError,
};

impl CompositeTypeDescriptor {
    /// Capture an expanded record's field binding without reading any tuple payload. Dropped physical positions retain their original width and alignment.
    pub fn expand_record_with_control(
        &self,
        value: &uqa_core::Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<uqa_core::Value>, crate::SQLError> {
        use crate::catalog::type_metadata::{
            pg_type_align, pg_type_len, pg_type_modifier, pg_type_oid,
        };
        use uqa_core::{RecordDatumField, Value};
        let Value::Datum(datum) = value else {
            return Err(crate::SQLError::Internal(
                "expanded record has no physical tuple".into(),
            ));
        };
        if datum.record_fields().is_some() {
            return Ok(control.copy_value(value)?);
        }
        let mut removed = ProductionVec::new(*control);
        removed.extend_from_slice(&self.dropped)?;
        removed
            .as_mut_slice()
            .sort_unstable_by_key(|attribute| attribute.number);
        let mut dropped = removed.iter().peekable();
        let mut live = self.attributes.iter().peekable();
        let mut fields = ProductionVec::new(*control);
        for number in 1..=self.physical_width() {
            control.check()?;
            let field = if let Some(attribute) =
                live.peek().filter(|attribute| attribute.number == number)
            {
                let (name, memory) = control.copy_text(&attribute.name)?.into_parts();
                let field = control.finish(
                    RecordDatumField {
                        name,
                        type_oid: pg_type_oid(&attribute.ty) as u32,
                        type_modifier: pg_type_modifier(&attribute.ty) as i32,
                        collation_oid: attribute.collation_oid,
                        length: pg_type_len(&attribute.ty),
                        alignment: pg_type_align(&attribute.ty).as_bytes()[0],
                        dropped: false,
                    },
                    memory,
                )?;
                live.next();
                field
            } else if let Some(attribute) = dropped
                .peek()
                .filter(|attribute| attribute.number == number)
            {
                let field = control.finish(
                    RecordDatumField {
                        name: String::new(),
                        type_oid: 0,
                        type_modifier: -1,
                        collation_oid: None,
                        length: attribute.length,
                        alignment: attribute.alignment,
                        dropped: true,
                    },
                    control.empty_reservation(),
                )?;
                dropped.next();
                field
            } else {
                return Err(crate::SQLError::Internal(
                    "expanded record descriptor is missing a physical attribute".into(),
                ));
            };
            fields.push_produced(field)?;
        }
        let (datum, memory) = datum
            .with_record_fields_with_control(fields.finish()?, control)?
            .into_parts();
        Ok(control.finish(Value::Datum(datum), memory)?)
    }

    /// Rebind a copied expanded record from its frozen field identities, without
    /// observing tuple payloads or replacing them with the named type's new layout.
    pub fn expanded_with_control(
        datum: &uqa_core::DatumValue,
        catalog: Option<&dyn crate::expr::SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, crate::SQLError> {
        let Some(fields) = datum.record_fields() else {
            return Ok(None);
        };
        let mut attributes = ProductionVec::new(*control);
        let mut dropped = ProductionVec::new(*control);
        for (index, field) in fields.iter().enumerate() {
            control.check()?;
            let number = i16::try_from(index + 1)
                .map_err(|_| crate::SQLError::Internal("invalid expanded record width".into()))?;
            if field.dropped {
                dropped.push_copy(super::DroppedCompositeAttribute {
                    number,
                    length: field.length,
                    alignment: field.alignment,
                })?;
            } else {
                let (ty, type_memory) = crate::expr::value_catalog::field_type_with_control(
                    field.type_oid,
                    field.type_modifier,
                    catalog,
                    control,
                )?
                .into_parts();
                let (name, name_memory) = control.copy_text(&field.name)?.into_parts();
                attributes.push_produced(control.finish(
                    CompositeAttribute {
                        name,
                        ty,
                        number,
                        collation_oid: field.collation_oid,
                    },
                    control.combine(name_memory, type_memory),
                )?)?;
            }
        }
        let (attributes, memory) = attributes.finish()?.into_parts();
        let (dropped, dropped_memory) = dropped.finish()?.into_parts();
        Ok(Some(control.finish(
            Self {
                type_oid: datum.type_oid(),
                relation_oid: 0,
                attributes,
                dropped,
            },
            control.combine(memory, dropped_memory),
        )?))
    }

    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let mut attributes = ProductionVec::new(*control);
        attributes.reserve(self.attributes.len())?;
        for attribute in &self.attributes {
            let (name, name_memory) = control.copy_text(&attribute.name)?.into_parts();
            let (ty, type_memory) = attribute.ty.clone_with_control(control)?.into_parts();
            attributes.push_produced(control.finish(
                CompositeAttribute {
                    name,
                    ty,
                    number: attribute.number,
                    collation_oid: attribute.collation_oid,
                },
                control.combine(name_memory, type_memory),
            )?)?;
        }
        let mut dropped = ProductionVec::new(*control);
        dropped.extend_from_slice(&self.dropped)?;
        let (attributes, memory) = attributes.finish()?.into_parts();
        let (dropped, dropped_memory) = dropped.finish()?.into_parts();
        control.finish(
            Self {
                attributes,
                dropped,
                type_oid: self.type_oid,
                relation_oid: self.relation_oid,
            },
            control.combine(memory, dropped_memory),
        )
    }
}
