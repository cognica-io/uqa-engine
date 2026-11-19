//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::comments::CommentAddress;
use std::collections::BTreeMap;
use uqa_sql::{ast::RelationPersistence, catalog::dependencies::*};
use uqa_storage::memory_capture::MemoryCaptureRegistry;

fn address(class_id: u32, object_id: u32, sub_id: i32) -> CommentAddress {
    CommentAddress {
        object_id,
        sub_id,
        class_id,
    }
}

fn fixture() -> CommentCaptureSource {
    use RelationPersistence::{Permanent, Temporary, Unlogged};
    let comments = CommentRegistry::try_from_iter([
        (
            address(RELATION_CLASS, 80_001, 0),
            "x".repeat(1 << 20),
            Temporary,
        ),
        (
            address(RELATION_CLASS, 80_001, 3),
            "temporary column".into(),
            Temporary,
        ),
        (
            address(TYPE_CLASS, 80_002, 0),
            "temporary row".into(),
            Temporary,
        ),
        (
            address(999, 80_001, 3),
            "opaque class 한글\n".into(),
            Permanent,
        ),
        (
            address(RELATION_CLASS, 90_001, 0),
            "permanent".into(),
            Permanent,
        ),
        (
            address(RELATION_CLASS, 90_002, 1),
            "unlogged column".into(),
            Unlogged,
        ),
        (
            address(RELATION_CLASS, 90_003, 0),
            "dangling".into(),
            Permanent,
        ),
        (
            address(NAMESPACE_CLASS, 90_004, 0),
            "temporary namespace".into(),
            Temporary,
        ),
        (
            address(NAMESPACE_CLASS, 90_005, 0),
            "temporary toast namespace".into(),
            Temporary,
        ),
    ])
    .unwrap();
    CommentCaptureSource::new(&comments)
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
    let expected = selected
        .iter()
        .map(|(address, text)| (*address, text.clone()))
        .collect::<BTreeMap<_, _>>();
    let selected_only = CommentCaptureSource {
        comments: CommentRegistry::try_from_iter(
            expected
                .clone()
                .into_iter()
                .map(|(address, text)| (address, text, RelationPersistence::Permanent)),
        )
        .unwrap(),
    };
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
    assert!(value.get().unwrap().iter().eq(expected.iter()));
    drop(source);
    assert!(value.get().unwrap().iter().eq(expected.iter()));
    drop(value);
    assert_eq!(exact.memory().used(), 0);
}

#[test]
fn comment_generation_retirement_preserves_independent_capture_controls() {
    let source = fixture();
    let native = source.comments.budget().clone();
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
    assert!(native.used() > 0);
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
    assert_eq!(native.used(), 0);
}

#[test]
fn native_comment_source_selection_is_allocation_free_and_releases_unselected_temporary_text() {
    use uqa_core::memory::MemoryBudget;
    for count in [1, 64, 4096] {
        let native = MemoryBudget::new(16 << 20);
        let mut comments = CommentRegistry::with_budget(&native);
        for id in 0..count {
            comments
                .insert(
                    address(TYPE_CLASS, id + 1, 0),
                    "original",
                    RelationPersistence::Permanent,
                )
                .unwrap();
            comments
                .insert(
                    address(TYPE_CLASS, id + 10_000, 0),
                    "temporary",
                    RelationPersistence::Temporary,
                )
                .unwrap();
        }
        comments
            .insert(
                address(TYPE_CLASS, 99_999, 0),
                &"x".repeat(4 << 20),
                RelationPersistence::Temporary,
            )
            .unwrap();
        let mut selected = None;
        let work = allocation_counter::measure(|| {
            selected = Some(CommentCaptureSource::new(&comments));
        });
        assert_eq!(work.count_total, 0, "{count}: {work:?}");
        let selected = selected.unwrap();
        let before = native.used();
        comments
            .insert(
                address(TYPE_CLASS, 1, 0),
                "replacement",
                RelationPersistence::Permanent,
            )
            .unwrap();
        drop(comments);
        assert!(before - native.used() >= 4 << 20);
        let control = StorageReadControl::with_limit(4 << 20);
        let captured = selected.freeze(&control).unwrap();
        drop(selected);
        assert_eq!(
            captured.get().unwrap().len(),
            usize::try_from(count).unwrap()
        );
        assert!(captured
            .get()
            .unwrap()
            .iter()
            .all(|(_, text)| text == "original"));
        drop(captured);
        assert_eq!(native.used(), 0);
        assert_eq!(control.memory().used(), 0);
    }
}
