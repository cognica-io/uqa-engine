//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn row(name: &str, payload: String) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", name),
        index_type: "btree".into(),
        table_name: "public.items".into(),
        columns_json: "[\"id\"]".into(),
        parameters_json: payload,
        definition_json: None,
    }
}

#[test]
fn registry_snapshots_share_payloads_and_release_with_the_last_reader() {
    let value = row("large", "x".repeat(4 * 1024 * 1024));
    let rows = Arc::new(BTreeMap::from([(value.relation.clone(), value)]));
    let pointer = rows.values().next().unwrap().parameters_json.as_ptr();
    let weak = Arc::downgrade(&rows);
    let snapshot = CatalogIndexSnapshot::from(rows);
    let allocation = allocation_counter::measure(|| {
        let readers: [_; 64] = std::array::from_fn(|_| snapshot.clone());
        for reader in &readers {
            assert_eq!(reader.len(), 1);
            assert_eq!(
                reader.iter().next().unwrap().parameters_json.as_ptr(),
                pointer
            );
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert!(weak.upgrade().is_some());
    let released = allocation_counter::measure(|| drop(snapshot));
    assert!(released.bytes_current <= -4 * 1024 * 1024, "{released:?}");
    assert!(weak.upgrade().is_none());
}

#[test]
fn catalog_writes_preserve_the_observed_generation_and_structural_order() {
    let a = row("a", "original".into());
    let z = row("z", "last".into());
    let mut rows = Arc::new(BTreeMap::from([
        (z.relation.clone(), z),
        (a.relation.clone(), a),
    ]));
    let before = CatalogIndexSnapshot::from(rows.clone());
    let writer = Arc::make_mut(&mut rows);
    writer.remove(&RelationIdentity::new("public", "a"));
    writer.values_mut().next().unwrap().parameters_json = "changed".into();
    let after = CatalogIndexSnapshot::from(rows);
    assert_eq!(
        before
            .iter()
            .map(|row| row.relation.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert_eq!(before.iter().next_back().unwrap().parameters_json, "last");
    assert_eq!(after.len(), 1);
    assert_eq!(after.iter().next().unwrap().parameters_json, "changed");
}

#[test]
fn supplied_sequences_keep_order_duplicates_and_payload_identity() {
    let rows = vec![
        row("z", "first".into()),
        row("a", "middle".into()),
        row("z", "last".into()),
    ];
    let pointer = rows[0].parameters_json.as_ptr();
    let snapshot = CatalogIndexSnapshot::from(rows);
    assert_eq!(
        snapshot
            .iter()
            .map(|row| row.relation.name.as_str())
            .collect::<Vec<_>>(),
        ["z", "a", "z"]
    );
    assert_eq!(
        snapshot.iter().next().unwrap().parameters_json.as_ptr(),
        pointer
    );
    assert_eq!(snapshot.iter().next_back().unwrap().parameters_json, "last");
    let mut iterator = snapshot.iter();
    assert_eq!(iterator.len(), 3);
    iterator.next();
    iterator.next_back();
    assert_eq!(iterator.len(), 1);
    assert_eq!(iterator.next().unwrap().parameters_json, "middle");
    assert!(iterator.next().is_none());
    let allocation = allocation_counter::measure(|| {
        let cloned = snapshot.clone();
        assert_eq!(cloned.len(), 3);
        let empty = CatalogIndexSnapshot::default();
        assert!(empty.is_empty());
        assert!(empty.clone().iter().next().is_none());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn unwinding_a_catalog_reader_releases_its_retained_rows() {
    let value = row("release", "owned".into());
    let rows = Arc::new(BTreeMap::from([(value.relation.clone(), value)]));
    let weak = Arc::downgrade(&rows);
    let snapshot = CatalogIndexSnapshot::from(rows);
    assert!(std::panic::catch_unwind(|| {
        let reader = snapshot;
        assert_eq!(reader.len(), 1);
        panic!("catalog reader failed");
    })
    .is_err());
    assert!(weak.upgrade().is_none());
}
