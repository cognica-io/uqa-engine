//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn fixed_catalog_descriptors_admit_nested_types_and_release_partial_construction() {
    for relation in VirtualRelation::ALL {
        let memory = MemoryBudget::new(128 << 10);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(relation.schema_with_control(&control).unwrap());
        });
        let output = output.unwrap();
        assert_eq!(
            allocation.bytes_current,
            memory.used() as i64,
            "{relation:?}"
        );
        assert_eq!(output.reserved_bytes(), memory.used());
        assert_eq!(
            output.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            relation
                .column_names_with_control(&control)
                .unwrap()
                .iter()
                .collect::<Vec<_>>()
        );
        drop(output);
        assert_eq!(memory.used(), 0);
        for available in [0, 127, 1023] {
            let small = memory.child(available);
            let selected = ProductionControl::new(&small, &original, &invoking);
            let result = relation.schema_with_control(&selected);
            if let Err(error) = &result {
                assert_eq!(error.sqlstate(), Some("53200"));
            }
            drop(result);
            assert_eq!(memory.used(), 0);
        }
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                relation
                    .schema_with_control(&control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), 0);
            token.reset();
        }
    }
}
