//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve exact encoded lengths while schemas or completed row batches remain unchanged.

use super::EncodedBatchSizer;
use crate::{Batch, ExecResult, PhysicalRow, RowSchema};

/// An unchanged sequence of physical rows can keep its validated record length across schema-only projections. The spill buffer still sizes the destination schema.
pub(crate) struct SpillBatchRows {
    rows: Vec<PhysicalRow>,
    records: Option<(usize, usize)>,
}

impl From<Vec<PhysicalRow>> for SpillBatchRows {
    fn from(rows: Vec<PhysicalRow>) -> Self {
        Self {
            rows,
            records: None,
        }
    }
}

impl SpillBatchRows {
    pub(crate) fn into_rows(self) -> Vec<PhysicalRow> {
        self.rows
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(crate) fn push_to(
        self,
        output: &mut super::SpillBuffer,
        schema: &RowSchema,
    ) -> ExecResult<bool> {
        let records = self
            .records
            .and_then(|(width, bytes)| (width == schema.physical_width()).then_some(bytes));
        output.push_batch(
            Batch::from_physical_rows(schema.clone(), self.rows),
            records,
        )
    }
}

impl EncodedBatchSizer {
    pub(crate) fn finish_rows(self, rows: Vec<PhysicalRow>) -> SpillBatchRows {
        debug_assert_eq!(self.origin_free_rows + self.lock_origin_rows, rows.len());
        SpillBatchRows {
            rows,
            records: Some((self.physical_width, self.bytes - self.header_bytes)),
        }
    }
}

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
    fn measured_records_use_the_destination_header_and_preserve_lock_origin_bytes() {
        let source = RowSchema::new(vec!["v".into()]);
        let destination = RowSchema::new(vec!["a longer destination label".into()]);
        let plain = PhysicalRow::from_values(vec![Value::Null]);
        let locked = PhysicalRow::from_values(vec![Value::Str("한".repeat(64))])
            .with_lock_origin(RowLockOrigin::new("t", "public.t", 9));
        let rows = vec![plain.clone(), locked, plain];
        let expected = Batch::from_physical_rows(destination.clone(), rows.clone());
        let bytes = super::super::SpillBuffer::encoded_size(&expected).unwrap();
        for budget in [bytes, bytes - 1] {
            let mut output = super::super::SpillBuffer::new(budget);
            let mut size = EncodedBatchSizer::new(&source).unwrap();
            for row in &rows {
                size.append(row).unwrap();
            }
            let wrote = size
                .finish_rows(rows.clone())
                .push_to(&mut output, &destination)
                .unwrap();
            assert_eq!(wrote, budget < bytes);
            if wrote {
                assert_eq!(output.spilled_rows(), rows.len());
                assert_eq!(output.in_memory_bytes(), 0);
            } else {
                assert_eq!(output.in_memory_bytes(), bytes);
            }
            assert_eq!(
                output.drain_all().unwrap().as_slice(),
                std::slice::from_ref(&expected)
            );
        }
    }

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
    fn borrowed_batch_sizing_matches_owned_values_before_and_after_lock_origins() {
        let schema = RowSchema::new(vec!["body".into(), "value".into()]);
        let values = [Value::Str("日本λ".repeat(128)), Value::Null];
        let borrowed = values.iter().collect::<Vec<_>>();
        let plain = PhysicalRow::from_values(values.to_vec());
        let locked = plain
            .clone()
            .with_lock_origin(RowLockOrigin::new("t", "public.t", 2));
        for locked_first in [false, true] {
            let mut rows = Vec::new();
            let mut size = EncodedBatchSizer::new(&schema).unwrap();
            for is_locked in [locked_first, !locked_first, false] {
                if is_locked {
                    size.append(&locked).unwrap();
                    rows.push(locked.clone());
                } else {
                    size.append_borrowed(&borrowed).unwrap();
                    rows.push(plain.clone());
                }
                assert_eq!(
                    size.bytes(),
                    super::super::encoded_batch_size(&Batch::from_physical_rows(
                        schema.clone(),
                        rows.clone()
                    ))
                    .unwrap()
                );
            }
            for row in rows.iter().rev() {
                size.remove(row).unwrap();
            }
            assert_eq!(
                size.bytes(),
                EncodedBatchSizer::new(&schema).unwrap().bytes()
            );
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
