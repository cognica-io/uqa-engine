//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::ColumnType, expr::composites::DroppedCompositeAttribute};
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn descriptor_payload_and_shared_lease_survive_each_reader() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let removed = DroppedCompositeAttribute::from_type(2, &ColumnType::Integer);
    let descriptor = CompositeTypeDescriptor::from_metadata_with_control(
        80001,
        80002,
        [
            Ok(("name", &ColumnType::Text, 1, Some(951))),
            Ok(("after_drop", &ColumnType::Integer, 3, None)),
        ],
        [Ok(removed)],
        &control,
    )
    .unwrap();
    let descriptor = OwnedCompositeTypeDescriptor::from_produced(descriptor, &control).unwrap();
    let bytes = descriptor.reserved_bytes();
    assert_eq!(budget.used(), bytes);
    assert_eq!(descriptor.physical_width(), 3);
    assert_eq!(descriptor.dropped, [removed]);
    let mut cloned = None;
    let allocation = allocation_counter::measure(|| {
        cloned = Some(descriptor.clone());
    });
    assert_eq!(allocation.count_total, 0);
    drop(descriptor);
    assert_eq!(budget.used(), bytes);
    assert_eq!(cloned.as_ref().unwrap().attributes[1].name, "after_drop");
    drop(cloned);
    assert_eq!(budget.used(), 0);
}

#[test]
fn selected_attribute_rejection_precedes_its_copy_and_reclaims_partial_fields() {
    let name = "attribute".repeat(16 * 1024);
    let budget = MemoryBudget::new(2048);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = budget.reserve(31).unwrap();
    let allocation = allocation_counter::measure(|| {
        let error = CompositeTypeDescriptor::from_metadata_with_control(
            1,
            2,
            [
                Ok(("first", &ColumnType::Integer, 1, None)),
                Ok((name.as_str(), &ColumnType::Text, 2, None)),
            ],
            [],
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(budget.used(), 31);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn descriptor_helper_uses_controlled_catalog_and_preserves_missing_type_error() {
    struct Controlled;
    impl CompositeTypeCatalog for Controlled {
        fn composite_type(&self, _: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
            panic!("ordinary descriptor callback was used")
        }
        fn composite_type_with_control(
            &self,
            _: u32,
            control: &ProductionControl<'_>,
        ) -> Result<Option<OwnedCompositeTypeDescriptor>, SQLError> {
            control.check()?;
            Ok(None)
        }
    }
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        descriptor_with_control(Some(&Controlled), 81, &control)
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
    token.cancel();
    assert_eq!(
        descriptor_with_control(Some(&Controlled), 81, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
}

#[test]
fn cold_descriptor_input_reads_each_native_field_in_its_own_window() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::catalog::CatalogValidation;

    const WIDTH: usize = 128;
    for stop in [None, Some(WIDTH / 2)] {
        let memory = MemoryBudget::new(128 << 10);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let visited = Arc::new(AtomicUsize::new(0));
        let fields = visited.clone();
        let owner = control.capture();
        let read = CompositeDescriptorInput::from_catalog(&control, move |mut work| async move {
            let control = owner.control();
            let integer = ColumnType::Integer;
            let attributes = (1..=i16::try_from(WIDTH).unwrap()).map(|number| {
                fields.fetch_add(1, Ordering::Relaxed);
                Ok(("field", &integer, number, None))
            });
            let value = CompositeTypeDescriptor::from_metadata_with_work(
                12,
                24,
                attributes,
                [],
                &control,
                &mut work,
            )
            .await?;
            OwnedCompositeTypeDescriptor::from_produced(value, &control).map(Some)
        })
        .unwrap();
        let owner = control.capture();
        let mut check = CatalogValidation::new(&control, move |mut work| async move {
            let value = read.required(12, &owner.control(), &mut work).await?;
            assert_eq!(value.attributes.len(), WIDTH);
            assert_eq!(value.attributes.last().unwrap().number, 128);
            Ok(())
        })
        .unwrap();
        loop {
            let before = visited.load(Ordering::Relaxed);
            if stop == Some(before) {
                token.cancel();
            }
            let next = check.advance(&control, 1);
            let after = visited.load(Ordering::Relaxed);
            assert!(after - before <= 1, "visited {before}..{after}");
            match next {
                Ok(Some(pending)) => check = pending,
                Ok(None) => {
                    assert_eq!(stop, None);
                    assert_eq!(after, WIDTH);
                    break;
                }
                Err(error) => {
                    assert_eq!(error.sqlstate(), Some("57014"));
                    assert_eq!(stop, Some(after));
                    break;
                }
            }
        }
        assert_eq!(memory.used(), 0);
    }
}
