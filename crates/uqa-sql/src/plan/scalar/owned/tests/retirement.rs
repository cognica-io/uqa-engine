//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn deep(control: &ProductionControl<'_>) -> OwnedScalarExpr {
    let mut value = OwnedScalarExpr::literal(
        control.copy_value(&Value::Str("leaf".into())).unwrap(),
        control,
    )
    .unwrap();
    for _ in 0..8192 {
        value = OwnedScalarExpr::cast(value, control.copy_text("text").unwrap(), false, control)
            .unwrap();
    }
    value
}

#[test]
fn deep_scalar_owner_retires_without_allocating_or_recursing() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let memory = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let value = deep(&control);
            let retained = memory.used();
            let measured = allocation_counter::measure(|| drop(value));
            assert_eq!(measured.count_total, 0);
            assert_eq!(measured.bytes_current, -i64::try_from(retained).unwrap());
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_scalar_freezing_releases_only_ownership_metadata_without_recursive_folds() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let memory = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let value = deep(&control);
            let before = memory.used();
            let mut frozen = None;
            let measured =
                allocation_counter::measure(|| frozen = Some(value.finish(&control).unwrap()));
            let frozen = frozen.unwrap();
            assert_eq!(measured.count_total, 0);
            assert!(memory.used() < before);
            assert_eq!(memory.used(), frozen.reserved_bytes());
            let (mut expression, lease) = frozen.into_parts();
            crate::ir::ownership::retire(&mut expression);
            drop((expression, lease));
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_scalar_rewrite_failure_retires_detached_child_and_remaining_siblings() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let memory = MemoryBudget::new(32 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let children = [deep(&control), deep(&control)];
            let value = OwnedScalarExpr::boolean(children.into_iter(), true, &control).unwrap();
            let result = value.map_child_with_parent(0, &control, |_, _child| {
                Err::<OwnedScalarExpr, crate::SQLError>(crate::SQLError::Internal(
                    "deliberate rewrite failure".into(),
                ))
            });
            assert!(result.is_err());
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn late_scalar_publication_cancellation_retires_deep_roots_before_their_leases() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for cancellation in 0..3 {
                let memory = MemoryBudget::new(16 << 20);
                let sibling = memory.reserve(37).unwrap();
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&memory, &original, &invoking);
                let value = deep(&control);
                let before = memory.used();
                let publication =
                    super::super::publication::Publication::new(value, &control).unwrap();
                assert!(memory.used() < before);
                let retained = memory.used() - 37;
                if cancellation == 1 {
                    original.cancel();
                } else if cancellation == 2 {
                    invoking.cancel();
                }
                let mut error = None;
                let measured = allocation_counter::measure(|| {
                    if cancellation == 0 {
                        drop(publication);
                    } else {
                        error = Some(
                            publication
                                .finish(&control)
                                .expect_err("cancelled publication"),
                        );
                    }
                });
                assert_eq!(measured.count_total, 0);
                assert_eq!(measured.bytes_current, -i64::try_from(retained).unwrap());
                if let Some(error) = error {
                    assert_eq!(crate::SQLError::from(error).sqlstate(), Some("57014"));
                }
                assert_eq!(memory.used(), 37);
                drop(sibling);
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
