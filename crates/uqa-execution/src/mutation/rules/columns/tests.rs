//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn rule_column_runs_preserve_uniform_empty_sparse_and_owned_replay() {
    for budget in [1, 16 * 1024] {
        for count in [0, 1, 129] {
            for uniform in [false, true] {
                let expected = |index| {
                    if uniform || index % 3 == 0 {
                        [BTreeSet::from(["a".into()]), BTreeSet::new()]
                    } else {
                        empty()
                    }
                };
                let columns = RuleColumnSets::from_rows(
                    (0..count).map(|index| Ok(expected(index))),
                    budget,
                    CancellationToken::new(),
                )
                .unwrap();
                assert_eq!(columns.has_required_columns(), count != 0);
                if count <= 1 || uniform {
                    assert!(matches!(columns.storage, Storage::Uniform(_)));
                } else if budget == 1 {
                    assert!(matches!(&columns.storage, Storage::Spill(rows) if rows.has_spilled()));
                }
                let first = columns.reader().unwrap();
                let second = columns.reader().unwrap();
                drop(columns);
                for reader in [first, second] {
                    let mut actual_count = 0;
                    for (index, actual) in reader.enumerate() {
                        assert_eq!(actual.unwrap(), expected(index));
                        actual_count += 1;
                    }
                    assert_eq!(actual_count, count);
                }
            }
        }
    }
    let empty_columns =
        RuleColumnSets::<2>::from_rows((0..128).map(|_| Ok(empty())), 1, CancellationToken::new())
            .unwrap();
    assert!(!empty_columns.has_required_columns());
    assert!(matches!(empty_columns.storage, Storage::Uniform(_)));
}

#[test]
fn rule_column_readers_validate_runs_counts_and_column_names() {
    for values in [
        Vec::new(),
        vec![encode_prepared_doc_id(0), Value::List(Vec::new())],
        vec![encode_prepared_doc_id(1), Value::Null],
        vec![encode_prepared_doc_id(1), Value::List(vec![Value::Int(1)])],
        vec![
            encode_prepared_doc_id(1),
            Value::List(vec![Value::Str("a".into()), Value::Str("a".into())]),
        ],
        vec![
            encode_prepared_doc_id(1),
            Value::List(Vec::new()),
            Value::Null,
        ],
    ] {
        assert!(decode::<1>(PhysicalRow::from_values(values)).is_err());
    }
    for count in [1, 3] {
        let mut columns = RuleColumnSets::from_rows(
            [Ok([BTreeSet::new()]), Ok([BTreeSet::from(["a".into()])])],
            1,
            CancellationToken::new(),
        )
        .unwrap();
        columns.count = count;
        let mut reader = columns.reader().unwrap();
        assert!(reader.next().unwrap().is_ok());
        if count == 3 {
            assert!(reader.next().unwrap().is_ok());
        }
        assert!(reader.next().unwrap().is_err());
        assert!(reader.next().is_none());
    }
}

#[test]
fn rule_column_metadata_finishes_before_consumption_and_propagates_cancellation() {
    let cancellation = CancellationToken::new();
    let failing = [Ok([BTreeSet::new()]), Err(invalid("projection failed"))];
    assert!(RuleColumnSets::from_rows(failing, 1, cancellation.clone())
        .err()
        .unwrap()
        .to_string()
        .contains("projection failed"));
    let columns =
        RuleColumnSets::from_rows([Ok([BTreeSet::new()])], 1, cancellation.clone()).unwrap();
    let mut reader = columns.reader().unwrap();
    cancellation.cancel();
    assert_eq!(
        reader.next().unwrap().unwrap_err().sqlstate(),
        Some("57014")
    );
    assert!(reader.next().is_none());
    assert_eq!(columns.reader().err().unwrap().sqlstate(), Some("57014"));
    assert_eq!(
        RuleColumnSets::<1>::from_rows([], 1, cancellation)
            .err()
            .unwrap()
            .sqlstate(),
        Some("57014")
    );
}
