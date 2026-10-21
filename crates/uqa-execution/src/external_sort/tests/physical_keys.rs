//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn physical_index_sort_keeps_core_order_and_opaque_values_across_merge_passes() {
    let values = [
        Value::Null,
        Value::Int(1),
        Value::Float(1.0),
        Value::Float(-0.0),
        Value::Decimal(uqa_core::DecimalValue::parse("1.000").unwrap()),
        Value::Float(f64::NAN),
        Value::Datum(uqa_core::DatumValue::new(23, 8, vec![1])),
        Value::Row(vec![Value::Null, Value::Int(4)].into()),
        Value::Str("한글".into()),
    ];
    for allowance in [0, 4096] {
        let schema = RowSchema::with_internal_relation_types(
            uqa_sql::ast::InternalRelationId::allocate(),
            vec![None; 2],
        );
        let mut expected = (0..73)
            .rev()
            .map(|id| (values[id % values.len()].clone(), u64::MAX - id as u64))
            .collect::<Vec<_>>();
        let rows = expected
            .iter()
            .map(|(value, id)| {
                PhysicalRow::from_values(vec![
                    Value::Bytes(id.to_be_bytes().to_vec()),
                    value.clone(),
                ])
            })
            .collect();
        expected.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        let mut sorted = ExternalSort::physical_columns(
            Box::new(PhysicalRowsScan {
                schema,
                rows: Some(rows),
            }),
            vec![1, 0],
            allowance,
            &uqa_core::CancellationToken::new(),
        )
        .unwrap();
        assert!(sorted.output_ordering().is_empty());
        sorted.open().unwrap();
        if allowance == 0 {
            assert!(sorted.merge_pass_count() > 1);
        }
        let mut expected = expected.into_iter();
        while let Some(batch) = sorted.next().unwrap() {
            for row in batch.rows {
                let (value, id) = expected.next().unwrap();
                assert_eq!(row.value(0), Some(&Value::Bytes(id.to_be_bytes().to_vec())));
                assert!(row.value(1).unwrap().has_same_representation(&value));
            }
        }
        assert!(expected.next().is_none());
        sorted.close().unwrap();
    }
}

#[test]
fn physical_index_sort_validates_slots_and_preserves_cancellation() {
    let source = || PhysicalRowsScan {
        schema: RowSchema::new(vec!["key".into()]),
        rows: Some(vec![PhysicalRow::from_values(vec![Value::Int(1)])]),
    };
    let cancellation = uqa_core::CancellationToken::new();
    assert!(ExternalSort::physical_columns(Box::new(source()), vec![1], 0, &cancellation).is_err());
    let mut sorted =
        ExternalSort::physical_columns(Box::new(source()), vec![0], 0, &cancellation).unwrap();
    cancellation.cancel();
    let error = crate::physical::physical_exec_error(sorted.open().unwrap_err());
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(sorted.initial_run_count(), 0);
}
