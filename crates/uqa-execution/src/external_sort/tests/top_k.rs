//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn bounded_top_k_never_spills_discarded_input_rows() {
    let not_a_directory = tempfile::NamedTempFile::new().unwrap();
    let rows = (0..10_000)
        .rev()
        .map(|value| row(value / 3, value))
        .collect();
    let mut operator = sort(rows, 4096, Some(7)).with_spill_directory(not_a_directory.path());
    let (_, output) = run_to_rows(&mut operator).unwrap();
    assert_eq!(int_column(&output, "key"), [0, 0, 0, 1, 1, 1, 2]);
    assert_eq!(int_column(&output, "input"), [2, 1, 0, 5, 4, 3, 8]);
    assert_eq!(operator.initial_run_count(), 1);
    assert_eq!(operator.merge_pass_count(), 0);
}

#[test]
fn bounded_top_k_matches_complete_sort_with_nulls_ties_and_variable_payloads() {
    for budget in [1, 512, 2048, 1_000_000] {
        for keep in [0, 1, 7, 100] {
            for descending in [false, true] {
                for nulls_first in [false, true] {
                    let rows = (0..73)
                        .map(|i| {
                            let key = if i % 11 == 0 {
                                Value::Null
                            } else {
                                Value::Int((i * 19) % 17)
                            };
                            BTreeMap::from([
                                ("key".into(), key),
                                (
                                    "input".into(),
                                    Value::Str("x".repeat((i % 9 * 64) as usize)),
                                ),
                            ])
                        })
                        .collect::<Vec<_>>();
                    let mut full = sort(rows.clone(), budget, None);
                    let mut keys = full.keys.to_vec();
                    keys[0].descending = descending;
                    keys[0].nulls_first = Some(nulls_first);
                    full.keys = crate::scalar::PreparedExpressions::sort_keys(keys);
                    let mut expected = run_to_rows(&mut full).unwrap().1;
                    expected.truncate(keep);
                    let mut bounded = sort(rows, budget, Some(keep));
                    bounded.keys.clone_from(&full.keys);
                    assert_eq!(run_to_rows(&mut bounded).unwrap().1, expected);
                }
            }
        }
    }
}

#[test]
fn replacing_rows_keeps_exact_origin_metadata_budget() {
    let schema = RowSchema::new(vec!["key".into()]);
    let plain = PhysicalRow::from_values(vec![Value::Int(1)]);
    let locked = PhysicalRow::from_values(vec![Value::Int(2)])
        .with_lock_origin(crate::RowLockOrigin::new("t", "public.t", 2));
    let mut size = EncodedBatchSizer::new(&schema).unwrap();
    let mut rows = vec![plain.clone(), locked.clone(), plain.clone(), locked.clone()];
    for row in &rows {
        size.append(row).unwrap();
    }
    for position in [0, 2, 1, 0] {
        size.remove(&rows.remove(position)).unwrap();
        assert_eq!(
            size.bytes(),
            SpillBuffer::encoded_size(&Batch::from_physical_rows(schema.clone(), rows.clone()))
                .unwrap()
        );
    }
    size.append(&plain).unwrap();
    assert_eq!(
        size.bytes(),
        SpillBuffer::encoded_size(&Batch::from_physical_rows(schema, vec![plain])).unwrap()
    );
}
