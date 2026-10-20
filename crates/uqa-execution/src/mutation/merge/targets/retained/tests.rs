//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mutation::{
    merge::actions::ensure_merge_target_is_modified_once, row_membership::PhysicalRowBuffer,
};

#[test]
fn merge_rechecked_targets_bound_wide_images_and_keep_exact_successors() {
    for allowance in [0, 64 * 1024] {
        let allocation = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(allowance);
            let mut targets = TargetChanges::new(&memory, CancellationToken::new());
            for id in 0..128 {
                let document = Document::from([
                    ("body".into(), Value::Str("가".repeat(8192))),
                    ("id".into(), Value::Int(i64::try_from(id).unwrap())),
                    ("null".into(), Value::Null),
                    ("bytes".into(), Value::Bytes(vec![0, 255])),
                ]);
                targets
                    .push(
                        ("original", id),
                        Some(("successor", u64::MAX - id)),
                        Some(document),
                    )
                    .unwrap();
            }
            for id in (0..128).rev() {
                let Some(RecheckedTarget::Refreshed((table, successor), document)) =
                    targets.lookup("original", id).unwrap()
                else {
                    panic!("missing retained target");
                };
                assert_eq!(table, "successor");
                assert_eq!(successor, u64::MAX - id);
                assert_eq!(document["body"], Value::Str("가".repeat(8192)));
                assert_eq!(document["id"], Value::Int(i64::try_from(id).unwrap()));
                assert_eq!(document["null"], Value::Null);
                assert_eq!(document["bytes"], Value::Bytes(vec![0, 255]));
            }
            assert!(targets.lookup("original", 128).unwrap().is_none());
            assert!(targets.lookup("successor", u64::MAX).unwrap().is_none());
        });
        assert!(
            allocation.bytes_max < 512 * 1024,
            "{allowance}: {allocation:?}"
        );
    }
}

#[test]
fn merge_target_rechecks_preserve_deleted_unchanged_and_prefix_identities() {
    let mut targets = TargetChanges::new(&MemoryBudget::new(1), CancellationToken::new());
    for table in ["", "a", "a\0", "aa", "b", "한국어"] {
        targets.push((table, 0), None, None).unwrap();
        targets
            .push(
                (table, u64::MAX),
                Some((table, u64::MAX)),
                Some(Document::from([("value".into(), Value::Int(7))])),
            )
            .unwrap();
    }
    for table in ["", "a", "a\0", "aa", "b", "한국어"] {
        assert!(matches!(
            targets.lookup(table, 0).unwrap(),
            Some(RecheckedTarget::Deleted)
        ));
        assert!(targets.lookup(table, 1).unwrap().is_none());
        let Some(RecheckedTarget::Refreshed((current, id), mut document)) =
            targets.lookup(table, u64::MAX).unwrap()
        else {
            panic!("missing same-identity refresh");
        };
        assert_eq!(current, table);
        assert_eq!(id, u64::MAX);
        document.insert("value".into(), Value::Int(99));
        let Some(RecheckedTarget::Refreshed(_, original)) =
            targets.lookup(table, u64::MAX).unwrap()
        else {
            panic!("missing replay");
        };
        assert_eq!(original["value"], Value::Int(7));
    }
    assert!(targets.lookup("unknown", 0).unwrap().is_none());
}

#[test]
fn merge_target_retention_preserves_cardinality_errors_and_cancellation() {
    let cancellation = CancellationToken::new();
    let mut rows = PhysicalRowBuffer::new(0, cancellation.clone(), "MERGE modified targets");
    ensure_merge_target_is_modified_once(&mut rows, "a", u64::MAX).unwrap();
    ensure_merge_target_is_modified_once(&mut rows, "aa", u64::MAX).unwrap();
    let error = ensure_merge_target_is_modified_once(&mut rows, "a", u64::MAX).unwrap_err();
    assert_eq!(error.sqlstate(), Some("21000"));
    assert!(error
        .to_string()
        .contains("MERGE command cannot affect row a second time"));
    let mut targets = TargetChanges::new(&MemoryBudget::new(0), cancellation.clone());
    targets.push(("a", 1), None, None).unwrap();
    cancellation.cancel();
    assert_eq!(
        targets.lookup("a", 1).err().unwrap().sqlstate(),
        Some("57014")
    );
    assert_eq!(
        targets.push(("b", 2), None, None).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(
        ensure_merge_target_is_modified_once(&mut rows, "b", 0)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
}
