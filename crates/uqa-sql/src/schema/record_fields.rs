//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statically known anonymous-record descriptors follow their physical value slots through query analysis.

use super::{Arc, ColumnType, RowSchema, SchemaMap};

/// A record's analyzed field types. Runtime values cannot distinguish SQL integer widths or a NULL field's declared type.
mod ownership;
#[cfg(test)]
mod ownership_tests;
pub use ownership::RecordFields;
pub(crate) use ownership::RecordMemory;

/// Comparison field types and a selectable tuple descriptor are independent. A set operation can select fields through its left input even when its rows do not share field types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordMetadata {
    pub types: Option<RecordFields>,
    pub descriptor: Option<Arc<RecordDescriptor>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecordFieldMetadata {
    pub collation_oid: u32,
    pub sql_unknown: bool,
}

/// A tuple shape available to parser field selection. Anonymous names are generated only when looked up, without allocating an `fN` string for every field.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RecordDescriptor {
    pub types: RecordFields,
    pub names: Option<Box<[String]>>,
    pub fields: Box<[RecordFieldMetadata]>,
    pub(crate) memory: ownership::RecordMemory,
}

impl RecordDescriptor {
    pub fn position(&self, name: &str) -> Option<usize> {
        self.names.as_ref().map_or_else(
            || {
                crate::type_resolution::field_selection::anonymous_field_position(
                    name,
                    self.types.len(),
                )
            },
            |names| names.iter().position(|candidate| candidate == name),
        )
    }
}

impl RowSchema {
    pub fn record_fields(&self, logical: usize) -> Option<&RecordFields> {
        self.slot(logical)
            .and_then(|slot| self.physical_record_fields(slot))
    }

    pub fn physical_record_fields(&self, slot: usize) -> Option<&RecordFields> {
        self.physical_record_metadata(slot)?.types.as_ref()
    }

    pub(crate) fn physical_record_metadata(&self, slot: usize) -> Option<&RecordMetadata> {
        self.index.cold.record_fields.get(&slot)
    }

    pub(crate) fn record_metadata(&self, logical: usize) -> Option<&RecordMetadata> {
        self.slot(logical)
            .and_then(|slot| self.index.cold.record_fields.get(&slot))
    }

    pub(crate) fn input_record_metadata(
        &self,
        expression: &crate::ScalarExpr,
    ) -> Option<&RecordMetadata> {
        let slot = match expression {
            crate::ScalarExpr::Column(name) => self.column_slot(name),
            crate::ScalarExpr::QualifiedColumn { qualifier, column } => {
                self.qualified_slot(qualifier, column)
            }
            crate::ScalarExpr::InternalColumn(column) => self.internal_slot(*column),
            crate::ScalarExpr::Position(position) => self.slot(*position),
            _ => None,
        }?;
        self.index.cold.record_fields.get(&slot)
    }

    /// Attach comparison field types to logical columns without declaring a selectable tuple descriptor or changing slot identity.
    pub fn with_record_fields(
        self,
        fields: impl IntoIterator<Item = (usize, RecordFields)>,
    ) -> Self {
        self.with_record_metadata(fields.into_iter().map(|(position, types)| {
            (
                position,
                RecordMetadata {
                    types: Some(types),
                    descriptor: None,
                },
            )
        }))
    }

    pub(crate) fn with_record_metadata(
        mut self,
        fields: impl IntoIterator<Item = (usize, RecordMetadata)>,
    ) -> Self {
        for (logical, fields) in fields {
            if let Some(slot) = self.slot(logical) {
                Arc::make_mut(&mut self.index)
                    .cold
                    .record_fields
                    .insert(slot, fields);
            }
        }
        self
    }

    pub(crate) fn with_physical_record_metadata(
        mut self,
        fields: impl IntoIterator<Item = (usize, RecordMetadata)>,
    ) -> Self {
        for (slot, fields) in fields {
            assert!(
                slot < self.physical_width(),
                "record metadata requires a physical slot"
            );
            Arc::make_mut(&mut self.index)
                .cold
                .record_fields
                .insert(slot, fields);
        }
        self
    }

    /// Preserve descriptors when a namespace boundary rebuilds the same logical columns in a compact physical row.
    pub fn with_record_fields_from(self, input: &Self) -> Self {
        let width = self.len().min(input.len());
        self.with_record_metadata((0..width).filter_map(|index| {
            input
                .record_metadata(index)
                .cloned()
                .map(|fields| (index, fields))
        }))
    }

    pub(super) fn joined_record_fields(
        left: &Self,
        right: &Self,
        right_base: usize,
    ) -> SchemaMap<usize, RecordMetadata> {
        let mut fields = left.index.cold.record_fields.clone();
        fields.extend(
            right
                .index
                .cold
                .record_fields
                .iter()
                .map(|(slot, fields)| (right_base + slot, fields.clone())),
        );
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_alias_join_and_compaction_preserve_record_field_slots() {
        let fields: RecordFields =
            vec![Some(ColumnType::Integer), Some(ColumnType::BigInteger)].into();
        let descriptor = Arc::new(RecordDescriptor {
            memory: RecordMemory::default(),
            types: fields.clone(),
            names: None,
            fields: vec![
                RecordFieldMetadata {
                    collation_oid: 0,
                    sql_unknown: false
                };
                2
            ]
            .into(),
        });
        let input = RowSchema::with_types(
            vec!["discard".into(), "value".into()],
            vec![Some(ColumnType::Text), Some(ColumnType::Record)],
        )
        .with_record_metadata([(
            1,
            RecordMetadata {
                types: Some(fields.clone()),
                descriptor: Some(descriptor.clone()),
            },
        )]);
        let selected = RowSchema::select(&input, &[("renamed".into(), "value".into())]);
        assert_eq!(selected.record_fields(0), Some(&fields));
        let qualified = RowSchema::with_relation_qualifier(&selected, "r");
        assert_eq!(
            qualified.physical_record_fields(qualified.qualified_slot("r", "renamed").unwrap()),
            Some(&fields)
        );
        let (compact, _) = qualified.canonical_projection();
        assert_eq!(compact.record_fields(0), Some(&fields));
        let public = qualified.for_result_columns(qualified.columns());
        assert_eq!(public.physical_width(), 1);
        assert_eq!(public.qualified_slot("r", "renamed"), None);
        assert_eq!(public.position("discard"), None);
        let outer = RowSchema::with_outer_schema(&RowSchema::default(), &compact);
        assert_eq!(
            outer.physical_record_fields(outer.qualified_slot("r", "renamed").unwrap()),
            Some(&fields)
        );
        for schema in [&selected, &qualified, &compact, &public] {
            assert!(Arc::ptr_eq(
                &descriptor,
                schema
                    .record_metadata(0)
                    .unwrap()
                    .descriptor
                    .as_ref()
                    .unwrap()
            ));
            assert!(fields.shares_storage(schema.record_fields(0).unwrap()));
        }
        assert!(Arc::ptr_eq(
            &descriptor,
            outer
                .physical_record_metadata(outer.qualified_slot("r", "renamed").unwrap())
                .unwrap()
                .descriptor
                .as_ref()
                .unwrap()
        ));
    }
}
