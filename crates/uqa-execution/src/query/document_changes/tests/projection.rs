//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::document_projection::read_document_projection;
use crate::query::exact_lookup::{ExactLookupOverlay, FieldPresence};

#[test]
fn projections_batch_sources_and_preserve_order_duplicates_and_early_stop() {
    let probe = Probe::new(&source());
    let mut changes = retained(&probe);
    changes.insert_shared(
        7,
        Some((
            Arc::new(document(70).into_fields()),
            DocumentMetadata::default(),
        )),
    );
    let ids = [4, 1, 7, 1, 4, 9, 99, 4];
    let mut visited = Vec::new();
    changes
        .for_each_fields_multi_ref_with_presence(
            &ids,
            &["key", "missing"],
            &mut |id, present, values| {
                visited.push((id, present, values[0].clone(), values[1].clone()));
                true
            },
        )
        .unwrap();
    assert_eq!(
        visited,
        vec![
            (4, true, Value::Int(40), Value::Null),
            (1, true, Value::Int(10), Value::Null),
            (7, true, Value::Int(70), Value::Null),
            (1, true, Value::Int(10), Value::Null),
            (4, true, Value::Int(40), Value::Null),
            (9, false, Value::Null, Value::Null),
            (99, false, Value::Null, Value::Null),
            (4, true, Value::Int(40), Value::Null),
        ]
    );
    assert_eq!(
        *probe.projections.lock(),
        vec![vec![4, 1], vec![1, 4], vec![4]]
    );
    probe.projections.lock().clear();
    let mut stopped = Vec::new();
    changes
        .for_each_fields_multi(&ids, &["key"], &mut |id, values| {
            stopped.push((id, values));
            stopped.len() < 2
        })
        .unwrap();
    assert_eq!(
        stopped,
        vec![(4, vec![Value::Int(40)]), (1, vec![Value::Int(10)])]
    );
    assert_eq!(*probe.projections.lock(), vec![vec![4, 1]]);
    let shared = changes
        .get_shared_fields(&[4, 1, 4], &["key"])
        .unwrap()
        .unwrap();
    for (row, expected) in shared.into_iter().zip([40, 10, 40]) {
        row.unwrap()
            .with_projected(|values| assert_eq!(values, &[&Value::Int(expected)]));
    }
    assert!(changes
        .get_shared_fields(&[4, 7], &["key"])
        .unwrap()
        .is_none());
}

#[test]
fn tuple_projections_read_metadata_without_decoding_unrelated_fields() {
    let uqa_sql::Statement::CreateTable(table) =
        uqa_sql::compile("CREATE TABLE t (key INT, opaque TEXT)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    let mut rows = source();
    let mut explicit = document(40);
    explicit.fields_mut().insert("xmin".into(), Value::Null);
    rows.put_stored(4, explicit).unwrap();
    let probe = Probe::new(&rows);
    let changes = retained(&probe);
    let fields = ["xmin", "key", "xmin"];
    let typed =
        read_document_projection(&changes, &[4, 1, 9, 99], &fields, &table.columns).unwrap();
    assert_eq!(
        typed,
        [
            (1, vec![Value::Int(41), Value::Int(10), Value::Int(41)]),
            (4, vec![Value::Int(41), Value::Int(40), Value::Int(41)]),
        ]
        .into()
    );
    let dynamic = read_document_projection(&changes, &[4, 1, 9], &fields, &[]).unwrap();
    assert_eq!(dynamic[&4], vec![Value::Null, Value::Int(40), Value::Null]);
    assert_eq!(dynamic[&1], typed[&1]);
    assert!(probe.copies.lock().is_empty());
}

#[test]
fn explicit_xmin_columns_and_requested_virtual_expressions_keep_their_row_type() {
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile(
        "CREATE TABLE t (key INT, xmin INT, calculated INT GENERATED ALWAYS AS (key + 1) VIRTUAL, unused INT GENERATED ALWAYS AS (10 / key) VIRTUAL)"
    ).unwrap().remove(0) else { unreachable!() };
    let mut row = document(0);
    row.fields_mut().insert("xmin".into(), Value::Int(900));
    let mut rows = MemoryDocumentStore::new();
    rows.put_stored(1, row).unwrap();
    let projected =
        read_document_projection(&rows, &[1], &["calculated", "xmin"], &table.columns).unwrap();
    assert_eq!(projected[&1], vec![Value::Int(1), Value::Int(900)]);
}

#[test]
fn private_exact_lookup_reads_only_key_fields_and_distinguishes_missing_from_null() {
    let mut rows = source();
    let mut explicit = document(40);
    explicit.fields_mut().insert("nullable".into(), Value::Null);
    rows.put_stored(4, explicit).unwrap();
    let changes = retained(&Probe::new(&rows));
    assert!(ExactLookupOverlay::masks(&changes, 9).unwrap());
    assert!(!ExactLookupOverlay::is_empty(&changes).unwrap());
    assert_eq!(
        changes
            .find_match(&["key".into()], &[Value::Int(40)], FieldPresence::Required)
            .unwrap(),
        Some(4)
    );
    assert_eq!(
        changes
            .find_match(
                &["nullable".into()],
                &[Value::Null],
                FieldPresence::Required
            )
            .unwrap(),
        Some(4)
    );
    assert_eq!(
        changes
            .find_match(
                &["nullable".into()],
                &[Value::Null],
                FieldPresence::MissingIsNull
            )
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        changes
            .find_match(
                &["key".into(), "nullable".into()],
                &[Value::Int(10), Value::Null],
                FieldPresence::Required
            )
            .unwrap(),
        None
    );
    let deleted = DocumentChanges::from(BTreeMap::from([(1, None)]));
    assert!(!ExactLookupOverlay::is_empty(&deleted).unwrap());
    assert!(DocumentStore::is_empty(&deleted).unwrap());
}

#[test]
fn retained_full_reads_batch_only_explicitly_requested_rows() {
    let mut probe = Probe::new(&source());
    probe.allow_copy = true;
    let mut changes = retained(&probe);
    changes.insert_shared(
        7,
        Some((
            Arc::new(document(70).into_fields()),
            DocumentMetadata::default(),
        )),
    );
    let rows = changes.get_stored_many(&[4, 1, 7, 4, 8, u64::MAX]).unwrap();
    assert_eq!(
        rows.keys().copied().collect::<Vec<_>>(),
        vec![1, 4, 7, u64::MAX]
    );
    assert_eq!(
        *probe.copies.lock(),
        vec![vec![4, 1], vec![4], vec![u64::MAX]]
    );
}
