//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reuse only the immutable schema portion of exact batch sizing.

use super::EncodedBatchSizer;
use crate::{ExecResult, RowSchema};

/// Retain one successful schema observation. Every returned sizer starts with no rows or lock origins; a different immutable schema is validated before replacing the retained layout.
#[derive(Default)]
pub(crate) struct EncodedBatchLayout {
    current: Option<(RowSchema, EncodedBatchSizer)>,
}

impl EncodedBatchLayout {
    pub(crate) fn for_schema(&mut self, schema: &RowSchema) -> ExecResult<EncodedBatchSizer> {
        if let Some((retained, empty)) = &self.current {
            if retained.shares_index(schema) {
                return Ok(*empty);
            }
        }
        let empty = EncodedBatchSizer::new(schema)?;
        self.current = Some((schema.clone(), empty));
        Ok(empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Batch, PhysicalRow, RowLockOrigin};
    use uqa_core::Value;
    use uqa_sql::ColumnType;

    #[test]
    fn retained_batch_layout_keeps_exact_sizes_and_independent_lock_origin_state() {
        let schema = RowSchema::with_types(vec!["id".into()], vec![Some(ColumnType::BigInteger)]);
        let plain = PhysicalRow::from_values(vec![Value::Int(1)]);
        let locked = PhysicalRow::from_values(vec![Value::Int(2)])
            .with_lock_origin(RowLockOrigin::new("t", "public.t", 2));
        let mut layout = EncodedBatchLayout::default();
        for rows in [
            vec![plain.clone(), locked.clone(), plain.clone()],
            vec![plain.clone()],
            vec![locked],
            Vec::new(),
        ] {
            let mut size = layout.for_schema(&schema.clone()).unwrap();
            for row in &rows {
                size.append(row).unwrap();
            }
            let expected = super::super::encoded_batch_size(&Batch::from_physical_rows(
                schema.clone(),
                rows.clone(),
            ))
            .unwrap();
            assert_eq!(size.bytes(), expected);
            for row in rows.iter().rev() {
                size.remove(row).unwrap();
            }
            assert_eq!(size.bytes(), layout.for_schema(&schema).unwrap().bytes());
        }
    }

    #[test]
    fn retained_batch_layout_rebinds_changed_schema_names_types_and_slots() {
        let plain = RowSchema::with_types(vec!["id".into()], vec![Some(ColumnType::BigInteger)]);
        let qualified = RowSchema::with_relation_qualifier(&plain, "long_relation_name");
        let different_type = RowSchema::with_types(
            vec!["body".into()],
            vec![Some(ColumnType::Varchar(Some(123)))],
        );
        let wider = RowSchema::with_types(
            vec!["id".into(), "body".into()],
            vec![Some(ColumnType::BigInteger), Some(ColumnType::Text)],
        );
        let mut layout = EncodedBatchLayout::default();
        for schema in [&plain, &qualified, &different_type, &wider, &plain] {
            let mut size = layout.for_schema(schema).unwrap();
            let row = PhysicalRow::from_values(vec![Value::Null; schema.physical_width()]);
            size.append(&row).unwrap();
            assert_eq!(
                size.bytes(),
                super::super::encoded_batch_size(&Batch::from_physical_rows(
                    schema.clone(),
                    vec![row]
                ),)
                .unwrap()
            );
        }
    }
}
