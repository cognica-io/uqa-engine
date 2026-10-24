//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;

fn fragmented_row(width: usize) -> (PhysicalRow, Vec<u8>) {
    let mut row = PhysicalRow::default();
    let mut expected = (width as u64).to_le_bytes().to_vec();
    for value in 0..width {
        row = PhysicalRow::concat_left_owned(
            row,
            &PhysicalRow::from_values(vec![Value::Int(value as i64)]),
        );
        expected.push(2);
        expected.extend_from_slice(&(value as i64).to_le_bytes());
    }
    (row, expected)
}

#[test]
fn fragmented_record_encoding_traverses_fragments_linearly() {
    for width in [16, 128, 512] {
        let (row, expected) = fragmented_row(width);
        let (encoded, widths) = PhysicalRow::count_fragment_width_reads(|| {
            encode_physical_row_record(&row, width).unwrap()
        });
        assert_eq!(encoded, expected);
        eprintln!("record: {width} fragments, {widths} width reads");
        assert!(
            widths <= 4 * width,
            "{width} fragments required {widths} width reads"
        );
    }
}

#[test]
fn fragmented_batch_encoding_traverses_fragments_linearly() {
    for width in [16, 128, 512] {
        let (row, expected) = fragmented_row(width);
        let schema = RowSchema::new((0..width).map(|slot| format!("field_{slot}")).collect());
        let batch = Batch::from_physical_rows(schema, vec![row]);
        let ((encoded, bytes), widths) = PhysicalRow::count_fragment_width_reads(|| {
            let bytes = encoded_batch_size(&batch).unwrap();
            let mut encoded = Vec::new();
            encode_batch(&mut encoded, &batch).unwrap();
            (encoded, bytes)
        });
        assert_eq!(encoded.len() + RECORD_PREFIX_BYTES, bytes);
        eprintln!("batch: {width} fragments, {widths} width reads");
        assert!(encoded.ends_with(&expected));
        let decoded = super::super::decode::decode_batch(&encoded).unwrap();
        assert_eq!(decoded.schema, batch.schema);
        assert_eq!(decoded.rows.len(), 1);
        for slot in 0..width {
            assert_eq!(decoded.rows[0].value(slot), Some(&Value::Int(slot as i64)));
        }
        assert!(
            widths <= 4 * width,
            "{width} fragments required {widths} width reads"
        );
    }
}

#[test]
fn fragmented_encoding_preserves_projection_and_record_boundaries() {
    let origin = crate::RowLockOrigin::new("items", "public.items", 7);
    let projected = PhysicalRow::from_shared_values(
        Arc::new(vec![Value::Int(11), Value::Int(22)]),
        Arc::from([1, usize::MAX, 0, 1]),
    )
    .append_values(vec![Value::Str("tail".into())])
    .with_lock_origin(origin.clone());
    for (row, expected) in [
        (
            projected,
            vec![
                Value::Int(22),
                Value::Null,
                Value::Int(11),
                Value::Int(22),
                Value::Str("tail".into()),
            ],
        ),
        (
            PhysicalRow::default().with_lock_origin(origin.clone()),
            vec![],
        ),
    ] {
        let width = expected.len();
        let record = encode_physical_row_record(&row, width).unwrap();
        let decoded = super::super::decode::decode_physical_row_record(&record, width).unwrap();
        assert_eq!(
            decoded.values().flatten().cloned().collect::<Vec<_>>(),
            expected
        );
        assert_eq!(decoded.lock_origins(), std::slice::from_ref(&origin));
        assert!(encode_physical_row_record(&row, width + 1)
            .unwrap_err()
            .to_string()
            .contains(&format!("missing slot {width} of {}", width + 1)));

        let schema = RowSchema::new((0..width).map(|slot| format!("field_{slot}")).collect());
        let batch = Batch::from_physical_rows(schema, vec![row.clone()]);
        let mut encoded = Vec::new();
        encode_batch(&mut encoded, &batch).unwrap();
        assert_eq!(
            encoded.len() + RECORD_PREFIX_BYTES,
            encoded_batch_size(&batch).unwrap()
        );
        let decoded = super::super::decode::decode_batch(&encoded).unwrap();
        assert_eq!(decoded.rows.len(), 1);
        assert_eq!(
            decoded.rows[0]
                .values()
                .flatten()
                .cloned()
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            decoded.rows[0].lock_origins(),
            std::slice::from_ref(&origin)
        );

        let malformed = Batch {
            schema: RowSchema::new((0..=width).map(|slot| format!("field_{slot}")).collect()),
            rows: vec![row],
        };
        assert!(encode_batch(&mut Vec::new(), &malformed)
            .unwrap_err()
            .to_string()
            .contains(&format!("missing slot {width} of {}", width + 1)));
    }
}
