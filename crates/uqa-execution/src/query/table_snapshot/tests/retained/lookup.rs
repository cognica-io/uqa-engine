//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_point_lookups_stop_after_the_first_matching_identity_page() {
    let mut source = MemoryDocumentStore::new();
    let count = u64::try_from(crate::DEFAULT_BATCH_SIZE * 2).unwrap();
    for id in 1..=count {
        source
            .put_stored(
                id,
                document(&[("key", Value::Int(i64::try_from(id).unwrap()))], 41),
            )
            .unwrap();
    }
    let probe = ProjectedSource::new(&source);
    let pages = Arc::clone(&probe.pages);
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        Arc::new(probe),
        &[],
        &schema(&[], &index),
        BTreeMap::new(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        view.documents
            .find_doc_id_by_field("key", &Value::Int(1))
            .unwrap(),
        Some(1)
    );
    assert_eq!(pages.swap(0, Ordering::Relaxed), 1);
    assert_eq!(
        view.documents
            .find_doc_id_by_fields(&["key".into()], &[Value::Int(1)])
            .unwrap(),
        Some(1)
    );
    assert_eq!(pages.swap(0, Ordering::Relaxed), 1);
    assert!(view.documents.has_value("key", &Value::Int(1)).unwrap());
    assert_eq!(pages.load(Ordering::Relaxed), 1);
    assert_eq!(view.documents.max_doc_id().unwrap(), count);
}

#[test]
fn retained_point_lookups_preserve_missing_fields_nulls_and_private_masks() {
    let mut source = MemoryDocumentStore::new();
    for (id, fields) in [
        (1, vec![]),
        (2, vec![("nullable", Value::Null)]),
        (3, vec![("key", Value::Int(3))]),
    ] {
        source.put_stored(id, document(&fields, 41)).unwrap();
    }
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let changes = [
        (2, None),
        (3, Some(document(&[("key", Value::Int(30))], 42))),
        (u64::MAX, Some(document(&[("nullable", Value::Null)], 43))),
    ]
    .into();
    let view = retain(
        Arc::new(ProjectedSource::new(&source)),
        &[],
        &schema(&[], &index),
        changes,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        view.documents
            .find_doc_id_by_field("nullable", &Value::Null)
            .unwrap(),
        Some(u64::MAX)
    );
    assert_eq!(
        view.documents
            .find_doc_id_by_fields(&["nullable".into()], &[Value::Null])
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        view.documents
            .find_doc_id_by_field("key", &Value::Int(3))
            .unwrap(),
        None
    );
    assert_eq!(
        view.documents
            .find_doc_id_by_field("key", &Value::Int(30))
            .unwrap(),
        Some(3)
    );
    assert_eq!(
        view.documents
            .find_doc_id_by_fields(
                &["key".into(), "nullable".into()],
                &[Value::Int(30), Value::Null]
            )
            .unwrap(),
        Some(3)
    );
    assert_eq!(
        view.documents.find_doc_id_by_fields(&[], &[]).unwrap(),
        None
    );
    assert_eq!(
        view.documents
            .find_doc_id_by_fields(&["key".into()], &[])
            .unwrap(),
        None
    );
    assert_eq!(view.documents.max_doc_id().unwrap(), u64::MAX);
}

#[test]
fn retained_identity_scans_cancel_before_reading_another_masked_page() {
    let mut source = MemoryDocumentStore::new();
    source.put_stored(1, document(&[], 41)).unwrap();
    source.put_stored(2, document(&[], 42)).unwrap();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    for masked in [false, true] {
        let cancellation = CancellationToken::new();
        let mut probe = ProjectedSource::new(&source);
        probe.cancel_after_page = Some(cancellation.clone());
        let pages = Arc::clone(&probe.pages);
        let changes = if masked {
            [(1, None)].into()
        } else {
            BTreeMap::new()
        };
        let view = retain(
            Arc::new(probe),
            &[],
            &schema(&[], &index),
            changes,
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            view.documents.next_doc_ids(None, 1),
            Err(uqa_storage::StorageBackendError::Cancelled(_))
        ));
        assert_eq!(pages.load(Ordering::Relaxed), 1);
        let nested = view.documents.snapshot().unwrap();
        assert!(matches!(
            nested.find_doc_id_by_field("key", &Value::Null),
            Err(uqa_storage::StorageBackendError::Cancelled(_))
        ));
        assert!(matches!(
            nested.max_doc_id(),
            Err(uqa_storage::StorageBackendError::Cancelled(_))
        ));
        assert_eq!(pages.load(Ordering::Relaxed), 1);
    }
}
