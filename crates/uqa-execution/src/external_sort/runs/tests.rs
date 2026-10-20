//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{RunList, SortedRun};
use crate::spill::{SpillBuffer, SpillFileArena};
use crate::{Batch, PhysicalRow, RowSchema};
use uqa_core::Value;

#[test]
fn sorted_run_descriptors_spill_without_retaining_per_run_owners() {
    let directory = tempfile::tempdir().unwrap();
    let schema = RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![None],
    );
    let count = 8192;
    let peak = allocation_counter::measure(|| {
        let arena = SpillFileArena::new(Some(directory.path().to_path_buf()));
        let mut runs = RunList::new(
            arena.clone(),
            schema.clone(),
            Some(directory.path().to_path_buf()),
        );
        for id in 0..count {
            let mut buffer = SpillBuffer::new(0).with_arena(&arena);
            buffer
                .push(Batch::from_physical_rows(
                    schema.clone(),
                    vec![PhysicalRow::from_values(vec![Value::Int(id)])],
                ))
                .unwrap();
            runs.push(SortedRun { buffer }).unwrap();
        }
        assert_eq!(runs.len(), usize::try_from(count).unwrap());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        let mut runs = runs.into_reader().unwrap();
        for id in 0..count {
            let mut run = runs.next().unwrap().unwrap();
            let mut rows = run.buffer.drain_rows().unwrap();
            assert_eq!(
                rows.next().unwrap().unwrap().row.value(0),
                Some(&Value::Int(id))
            );
            assert!(rows.next().is_none());
        }
        assert!(runs.next().unwrap().is_none());
    });
    assert!(peak.bytes_max <= 512 * 1024, "{peak:?}");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
