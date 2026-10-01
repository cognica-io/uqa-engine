//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;

fn schema() -> RowSchema {
    RowSchema::new(vec!["id".into(), "payload".into()])
}

fn row(id: i64, payload: Value) -> PhysicalRow {
    PhysicalRow::from_values(vec![Value::Int(id), payload])
}

#[test]
fn buffered_and_disk_rows_preserve_positions_values_and_float_bits() {
    for budget in [0, 64, 1024, 65536] {
        let mut buffered = BufferedIndexedSpill::new(schema(), budget);
        let mut disk = IndexedSpill::new(schema()).unwrap();
        for id in 0..32 {
            let value = row(
                id,
                Value::List(vec![Value::Float(-0.0), Value::Str(format!("row-{id}"))]),
            );
            buffered.push(&value).unwrap();
            disk.push(&value).unwrap();
        }
        assert_eq!(buffered.len(), disk.len());
        assert!(!buffered.is_empty());
        assert!(buffered.rows.budget().peak() <= budget);
        assert_eq!(buffered.disk.is_none(), budget == 65536);
        for index in [31, 0, 16, 2, 31] {
            let actual = buffered.get(index).unwrap();
            let expected = disk.get(index).unwrap();
            assert_eq!(actual.value(0), expected.value(0));
            assert_eq!(actual.value(1), expected.value(1));
            let Value::List(values) = actual.value(1).unwrap() else {
                panic!("list value lost");
            };
            let Value::Float(value) = values[0] else {
                panic!("float value lost");
            };
            assert_eq!(value.to_bits(), (-0.0_f64).to_bits());
        }
        assert!(buffered
            .get(32)
            .unwrap_err()
            .to_string()
            .contains("outside"));
    }
}

#[test]
fn an_oversized_row_spills_the_complete_prefix_and_releases_memory() {
    const SECRET: &str = "buffered-window-partition-secret";
    let mut buffered = BufferedIndexedSpill::new(schema(), 1024);
    let memory = buffered.rows.budget().clone();
    for id in 0..3 {
        buffered.push(&row(id, Value::Null)).unwrap();
    }
    assert!(buffered.disk.is_none());
    assert!(memory.used() > 0);
    let secret = SECRET.repeat(1024);
    buffered.push(&row(3, Value::Str(secret.clone()))).unwrap();
    assert_eq!(memory.used(), 0);
    assert!(memory.peak() <= memory.limit());
    let disk = buffered.disk.as_ref().unwrap();
    let paths = [disk.data.path().to_owned(), disk.offsets.path().to_owned()];
    let bytes = std::fs::read(&paths[0]).unwrap();
    assert!(!bytes
        .windows(SECRET.len())
        .any(|part| part == SECRET.as_bytes()));
    for index in 0..3 {
        let actual = buffered.get(index).unwrap();
        assert_eq!(actual.value(0), Some(&Value::Int(index as i64)));
        assert_eq!(actual.value(1), Some(&Value::Null));
    }
    assert_eq!(buffered.get(3).unwrap().value(1), Some(&Value::Str(secret)));
    drop(buffered);
    assert!(paths.iter().all(|path| !path.exists()));
}

#[test]
fn invalid_rows_preserve_the_previous_prefix_in_both_representations() {
    for budget in [0, 65536] {
        let mut buffered = BufferedIndexedSpill::new(schema(), budget);
        buffered.push(&row(1, Value::Null)).unwrap();
        assert!(buffered
            .push(&PhysicalRow::from_values(vec![Value::Int(9)]))
            .is_err());
        assert_eq!(buffered.len(), 1);
        assert_eq!(buffered.get(0).unwrap().value(0), Some(&Value::Int(1)));
        buffered.push(&row(2, Value::Null)).unwrap();
        assert_eq!(buffered.len(), 2);
        assert_eq!(buffered.get(1).unwrap().value(0), Some(&Value::Int(2)));
    }
}
