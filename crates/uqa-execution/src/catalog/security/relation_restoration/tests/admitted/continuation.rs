//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

type Input = ImageDecoder<Budgeted<BoundTableSecurity>, StorageBackendError>;

fn input(
    row: Budgeted<RelationSecurityRow>,
    roles: &Arc<RoleDefinitions>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let roles = roles.clone();
    let invoking = invoking.clone();
    ImageDecoder::new(memory, original, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(input.memory, input.cancellation, &invoking);
        restore_security_from_input(row, Some(["id"].into_iter()), &roles, &control, &mut input)
            .await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> StorageBackendResult<Option<Budgeted<BoundTableSecurity>>> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn retained_relation_restoration_preserves_original_authority_through_work_windows() {
    let roles = Arc::new(fixture().1);
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let row = admitted(&memory);
    let RelationSecurityRow::Bound(value) = &*row else {
        unreachable!()
    };
    let acl = value.acl.as_ref().unwrap().as_ptr();
    let (name, paths) = value.column_acls.iter().next().unwrap();
    let name = std::ptr::from_ref(name);
    let paths = paths.as_ptr();
    let retained = memory.used();
    let expected = restore_security(&row, Some(&["id".into()]), &roles, false).unwrap();
    let mut input = input(row, &roles, &memory, &original, &invoking);
    let mut pending = 0;
    let value = loop {
        if let Some(value) = advance(&mut input).unwrap() {
            break value;
        }
        pending += 1;
        assert!(pending < 1000);
    };
    assert!(pending > 16);
    assert_eq!(*value, expected);
    assert_eq!(value.acl.as_ref().unwrap().as_ptr(), acl);
    let (column, entries) = value.column_acls.iter().next().unwrap();
    assert_eq!(std::ptr::from_ref(column), name);
    assert_eq!(entries.as_ptr(), paths);
    assert_eq!(memory.used(), retained);
    assert_eq!(Arc::strong_count(&roles), 1);
    original.cancel();
    invoking.cancel();
    drop(input);
    assert_eq!(*value, expected);
    drop(value);
    assert_eq!(memory.used(), 0);
}

#[test]
fn retained_relation_restoration_retires_authority_at_every_pending_boundary() {
    let roles = Arc::new(fixture().1);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut baseline = input(admitted(&memory), &roles, &memory, &token, &token);
    let mut pending = 0;
    while advance(&mut baseline).unwrap().is_none() {
        pending += 1;
    }
    assert_eq!(memory.used(), 0);
    for stop in 0..=pending {
        for cancel in [None, Some(false), Some(true)] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = input(admitted(&memory), &roles, &memory, &original, &invoking);
            for _ in 0..stop {
                assert!(advance(&mut input).unwrap().is_none());
            }
            if let Some(original_owner) = cancel {
                if original_owner {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert!(matches!(
                    advance(&mut input),
                    Err(StorageBackendError::Cancelled(_))
                ));
            }
            drop(input);
            assert_eq!(memory.used(), 0, "stop {stop}, cancel {cancel:?}");
            assert_eq!(Arc::strong_count(&roles), 1);
        }
    }
}
