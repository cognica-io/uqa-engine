//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const DEPTH: usize = 10_000;

fn chain(domains: bool) -> ColumnType {
    let mut value = ColumnType::Character(17);
    for index in 0..DEPTH {
        value = if domains && index % 2 == 0 {
            ColumnType::Domain {
                schema: "public".into(),
                name: "nested".into(),
                oid: 65_536 + index as u32,
                array_oid: Some(100_000 + index as u32),
                collation_oid: Some(950),
                base: Box::new(value),
            }
        } else {
            ColumnType::Array(Box::new(value))
        };
    }
    value
}

fn check_chain(mut value: &ColumnType, domains: bool, modified: bool) {
    for index in (0..DEPTH).rev() {
        value = match value {
            ColumnType::Array(child) => {
                assert!(!domains || index % 2 == 1);
                child
            }
            ColumnType::Domain {
                schema,
                name,
                oid,
                array_oid,
                collation_oid,
                base,
            } => {
                assert!(domains && index % 2 == 0);
                assert_eq!(schema, "public");
                assert_eq!(name, "nested");
                assert_eq!(*oid, 65_536 + index as u32);
                assert_eq!(*array_oid, Some(100_000 + index as u32));
                assert_eq!(*collation_oid, Some(950));
                base
            }
            _ => panic!("type chain ended before its declared leaf"),
        };
    }
    assert_eq!(
        *value,
        if modified {
            ColumnType::Bpchar
        } else {
            ColumnType::Character(17)
        }
    );
}

#[test]
fn deep_type_copies_and_retirement_use_constant_stack_and_exact_payload_leases() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for domains in [false, true] {
                let original = chain(domains);
                let ordinary = original.clone();
                check_chain(&ordinary, domains, false);
                let budget = MemoryBudget::new(4 << 20);
                let token = CancellationToken::new();
                let control = ProductionControl::new(&budget, &token, &token);
                control.check().unwrap();
                let mut copied = None;
                let allocated = allocation_counter::measure(|| {
                    copied = Some(original.clone_with_control(&control).unwrap());
                });
                let copied = copied.unwrap();
                check_chain(&copied, domains, false);
                assert_eq!(copied.reserved_bytes(), budget.used());
                assert_eq!(budget.used() as i64, allocated.bytes_current);
                assert_eq!(allocated.bytes_total as i64, allocated.bytes_current);
                let freed = allocation_counter::measure(|| drop((copied, ordinary, original)));
                assert_eq!(freed.count_total, 0);
                assert_eq!(budget.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_type_copy_failure_retires_every_admitted_partial_node() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let original = chain(true);
            let token = CancellationToken::new();
            for available in [0, 64, 1024, 300_000] {
                let budget = MemoryBudget::new(available);
                let control = ProductionControl::new(&budget, &token, &token);
                control.check().unwrap();
                let allocated = allocation_counter::measure(|| {
                    let copied = original.clone_with_control(&control);
                    assert!(matches!(copied, Err(ValueRetentionError::Memory(_))));
                });
                assert_eq!(allocated.bytes_current, 0);
                assert_eq!(budget.used(), 0);
                if available == 300_000 {
                    assert!(budget.peak() > 200_000);
                }
            }
            check_chain(&original, true, false);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_array_modifier_removal_preserves_shape_and_domain_base_modifiers() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let budget = MemoryBudget::new(4 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            for domains in [false, true] {
                let original = chain(domains);
                assert_eq!(original.has_type_modifiers(), !domains);
                let copy = original
                    .without_type_modifiers_with_control(&control)
                    .unwrap();
                check_chain(&copy, domains, !domains);
                check_chain(&original, domains, false);
                drop(copy);
                assert_eq!(budget.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
