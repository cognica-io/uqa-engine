//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statically known anonymous-record descriptors follow their physical value slots through query analysis.

use super::{Arc, ColumnType, HashMap, RowSchema};

/// A record's analyzed field types. Runtime values cannot distinguish SQL integer widths or a NULL field's declared type.
pub type RecordFields = Arc<[Option<ColumnType>]>;

impl RowSchema {
    pub fn record_fields(&self, logical: usize) -> Option<&RecordFields> {
        self.slot(logical)
            .and_then(|slot| self.physical_record_fields(slot))
    }

    pub fn physical_record_fields(&self, slot: usize) -> Option<&RecordFields> {
        self.index.cold.record_fields.get(&slot)
    }

    /// Attach analyzed descriptors to logical columns without changing their type or slot identity.
    pub fn with_record_fields(
        mut self,
        fields: impl IntoIterator<Item = (usize, RecordFields)>,
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

    /// Preserve descriptors when a namespace boundary rebuilds the same logical columns in a compact physical row.
    pub fn with_record_fields_from(self, input: &Self) -> Self {
        let width = self.len().min(input.len());
        self.with_record_fields((0..width).filter_map(|index| {
            input
                .record_fields(index)
                .cloned()
                .map(|fields| (index, fields))
        }))
    }

    pub(super) fn joined_record_fields(
        left: &Self,
        right: &Self,
        right_base: usize,
    ) -> HashMap<usize, RecordFields> {
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
        let input = RowSchema::with_types(
            vec!["discard".into(), "value".into()],
            vec![Some(ColumnType::Text), Some(ColumnType::Record)],
        )
        .with_record_fields([(1, fields.clone())]);
        let selected = RowSchema::select(&input, &[("renamed".into(), "value".into())]);
        assert_eq!(selected.record_fields(0), Some(&fields));
        let qualified = RowSchema::with_relation_qualifier(&selected, "r");
        assert_eq!(
            qualified.physical_record_fields(qualified.qualified_slot("r", "renamed").unwrap()),
            Some(&fields)
        );
        let (compact, _) = qualified.canonical_projection();
        assert_eq!(compact.record_fields(0), Some(&fields));
        let outer = RowSchema::with_outer_schema(&RowSchema::default(), &compact);
        assert_eq!(
            outer.physical_record_fields(outer.qualified_slot("r", "renamed").unwrap()),
            Some(&fields)
        );
    }
}
