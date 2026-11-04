//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::empty_catalog;
use uqa_storage::memory_capture::MemoryCaptureRegistry;

fn address(class_id: u32, object_id: u32, sub_id: i32) -> CommentAddress {
    CommentAddress {
        object_id,
        sub_id,
        class_id,
    }
}

fn fixture() -> CommentCaptureSource {
    let mut definitions = empty_catalog().snapshot().definitions.clone();
    let relation = RelationIdentity::new("public", "temporary_membership");
    let permanent = RelationIdentity::new("pg_temp_99", "permanent_membership");
    let unlogged = RelationIdentity::new("public", "unlogged_membership");
    let legacy = RelationCatalogOids::legacy(RelationOidKind::Table, &[1; 16]);
    definitions.comments = Arc::new(CommentRegistry::from([
        (
            address(RELATION_CLASS, legacy.relation, 0),
            "x".repeat(1 << 20),
        ),
        (
            address(RELATION_CLASS, legacy.relation, 3),
            "temporary column".into(),
        ),
        (
            address(TYPE_CLASS, legacy.row_type.unwrap(), 0),
            "temporary row".into(),
        ),
        (
            address(999, legacy.relation, 3),
            "opaque class 한글\n".into(),
        ),
        (address(RELATION_CLASS, 90_001, 0), "permanent".into()),
        (address(RELATION_CLASS, 90_002, 1), "unlogged column".into()),
        (address(RELATION_CLASS, 90_003, 0), "dangling".into()),
        (
            address(NAMESPACE_CLASS, 90_004, 0),
            "temporary namespace".into(),
        ),
        (
            address(NAMESPACE_CLASS, 90_005, 0),
            "temporary toast namespace".into(),
        ),
    ]));
    let hierarchy = TableHierarchy::default();
    let tables = [
        (&relation, RelationPersistence::Temporary, None),
        (&permanent, RelationPersistence::Permanent, Some(90_001)),
        (&unlogged, RelationPersistence::Unlogged, Some(90_002)),
    ]
    .map(|(relation, persistence, oid)| CommentTableSource {
        relation,
        object_id: [1; 16],
        persistence,
        catalog_oids: oid.map(|relation| RelationCatalogOids {
            relation,
            row_type: None,
            array_type: None,
            rule: None,
        }),
        columns: &[],
        checks: &[],
        keys: &[],
        foreign_keys: &[],
        hierarchy: &hierarchy,
    });
    CommentCaptureSource::new(
        &definitions,
        tables,
        Some(TemporaryNamespaceOids {
            namespace: 90_004,
            toast_namespace: 90_005,
        }),
    )
    .unwrap()
}

#[test]
fn comment_selection_excludes_original_temporary_addresses_before_payload_admission() {
    let source = fixture();
    let control = StorageReadControl::with_limit(4096);
    let mut captured = None;
    let allocated =
        allocation_counter::measure(|| captured = Some(source.freeze(&control).unwrap()));
    let captured = captured.unwrap();
    let selected = captured.get().unwrap();
    assert_eq!(selected.len(), 4);
    assert!(selected
        .iter()
        .any(|(address, text)| address.class_id == 999 && text == "opaque class 한글\n"));
    assert!(selected
        .iter()
        .any(|(address, _)| address.object_id == 90_002 && address.sub_id == 1));
    assert!(selected
        .iter()
        .any(|(address, _)| address.object_id == 90_003));
    let expected = selected.clone();
    let mut selected_only = fixture();
    Arc::make_mut(&mut selected_only.comments)
        .retain(|address, _| !selected_only.excluded.contains(address));
    let other = StorageReadControl::with_limit(4096);
    let mut value = None;
    let without_excluded =
        allocation_counter::measure(|| value = Some(selected_only.freeze(&other).unwrap()));
    assert_eq!(allocated.bytes_total, without_excluded.bytes_total);
    assert_eq!(allocated.count_total, without_excluded.count_total);
    assert_eq!(control.memory().used(), other.memory().used());
    let required = control.memory().used();
    drop((captured, value));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(other.memory().used(), 0);
    let denied = StorageReadControl::with_limit(required - 1);
    assert!(source.freeze(&denied).is_err());
    assert_eq!(denied.memory().used(), 0);
    let exact = StorageReadControl::with_limit(required);
    let value = source.freeze(&exact).unwrap();
    assert_eq!(*value.get().unwrap(), expected);
    drop(source);
    assert_eq!(*value.get().unwrap(), expected);
    drop(value);
    assert_eq!(exact.memory().used(), 0);
}

#[test]
fn comment_generation_retirement_preserves_independent_capture_controls() {
    let source = fixture();
    let weak = Arc::downgrade(&source.comments);
    let registry = MemoryCaptureRegistry::new();
    let cell = registry.cell(source).unwrap();
    let key = cell.key().unwrap();
    let cancelled = StorageReadControl::with_limit(4096);
    let failed = registry.capture(&cancelled).unwrap();
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    assert!(key.retained(&cut).unwrap().is_none());
    cancelled.cancellation().cancel();
    drop(cell);
    assert!(weak.upgrade().is_none());
    assert!(failed.check().is_err());
    drop(failed);
    assert_eq!(cancelled.memory().used(), 0);
    let captured = key.captured(&cut).unwrap();
    assert_eq!(captured.get().unwrap().len(), 4);
    drop((cut, registry, key));
    control.cancellation().cancel();
    assert!(captured.get().is_err());
    drop(captured);
    assert_eq!(control.memory().used(), 0);
}
