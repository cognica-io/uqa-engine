//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The position sidecar across attachments, failures and orderly restarts.

use super::*;

mod peer;

fn key(sequence: u32) -> SequencePositionKey {
    let mut object = [0_u8; 16];
    object[..4].copy_from_slice(&sequence.to_be_bytes());
    object[15] = 1;
    SequencePositionKey {
        object,
        definition: [7; 16],
    }
}

fn position(current: i64) -> SequencePosition {
    SequencePosition {
        logged: (current + 32, true),
        current,
        called: true,
        log_count: 32,
    }
}

/// The positions' lock, held until dropped.
struct Locked<'a>(&'a FileLockCoordinator);

impl<'a> Locked<'a> {
    fn new(coordinator: &'a FileLockCoordinator) -> Self {
        coordinator.lock_sequence_positions().unwrap();
        Self(coordinator)
    }

    fn read(&self, sequence: u32) -> SequenceSlot {
        self.0.read_sequence_slot(&key(sequence)).unwrap()
    }

    fn recorded(&self, sequence: u32) -> Option<(i64, bool)> {
        self.read(sequence)
            .recorded
            .map(|recorded| (recorded.position.current, recorded.fresh))
    }

    fn record(&self, sequence: u32, current: i64) -> bool {
        self.0
            .record_sequence_position(&key(sequence), &position(current))
            .unwrap()
    }

    fn remove(&self, sequence: u32) {
        self.0.remove_sequence_position(&key(sequence)).unwrap();
    }

    fn header(&self) -> Option<Header> {
        self.0.stored_sequence_header().unwrap()
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        self.0.unlock_sequence_positions();
    }
}

/// Sequences whose slots begin at the same home.
fn colliding(count: usize) -> Vec<u32> {
    let mut homes = std::collections::HashMap::<u64, Vec<u32>>::new();
    for sequence in 0..1_000_000 {
        let run = homes.entry(home(&key(sequence))).or_default();
        run.push(sequence);
        if run.len() == count {
            return run.clone();
        }
    }
    panic!("no {count} sequences share a home");
}

#[test]
fn a_position_is_recorded_read_and_removed() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("one.db")).unwrap();
    let positions = Locked::new(&coordinator);
    let empty = positions.read(1);
    assert!(empty.recorded.is_none() && empty.recordable && !empty.crowded);
    assert!(positions.record(1, 10));
    assert!(positions.record(2, 20));
    assert_eq!(positions.recorded(1), Some((10, true)));
    assert!(positions.record(1, 11));
    assert_eq!(
        positions.read(1).recorded.unwrap().position,
        SequencePosition {
            logged: (43, true),
            current: 11,
            called: true,
            log_count: 32
        }
    );
    assert_eq!(positions.header().unwrap().used, 2);
    let mut all = coordinator
        .read_sequence_positions()
        .unwrap()
        .into_iter()
        .map(|(key, recorded)| (key, recorded.position.current, recorded.fresh))
        .collect::<Vec<_>>();
    all.sort_unstable_by_key(|(_, current, _)| *current);
    assert_eq!(all, [(key(1), 11, true), (key(2), 20, true)]);
    positions.remove(1);
    positions.remove(1);
    assert_eq!(positions.recorded(1), None);
    assert_eq!(positions.recorded(2), Some((20, true)));
}

#[test]
fn a_removed_slot_keeps_the_positions_after_it_reachable() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("runs.db")).unwrap();
    let positions = Locked::new(&coordinator);
    let run = colliding(4);
    for (value, sequence) in (1..).zip(&run[..3]) {
        assert!(positions.record(*sequence, value));
    }
    assert_eq!(positions.header().unwrap().used, 3);
    // A slot inside the run is only marked, so the probe still reaches the position after it.
    positions.remove(run[1]);
    assert_eq!(positions.header().unwrap().used, 3);
    assert_eq!(positions.recorded(run[1]), None);
    assert_eq!(positions.recorded(run[2]), Some((3, true)));
    // The marked slot is taken again before a never used one.
    assert!(positions.record(run[3], 4));
    assert_eq!(positions.header().unwrap().used, 3);
    assert_eq!(positions.recorded(run[2]), Some((3, true)));
    assert_eq!(positions.recorded(run[3]), Some((4, true)));
    // Removing the end of a run returns it and the marked slots before it to never used.
    positions.remove(run[3]);
    assert_eq!(positions.header().unwrap().used, 3);
    positions.remove(run[2]);
    assert_eq!(positions.header().unwrap().used, 1);
    assert_eq!(positions.recorded(run[0]), Some((1, true)));
    positions.remove(run[0]);
    assert_eq!(positions.header().unwrap().used, 0);
    assert_eq!(coordinator.read_sequence_positions().unwrap().len(), 0);
}

#[test]
fn an_orderly_restart_keeps_positions_for_their_sequences_to_confirm() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("restart.db");
    {
        let coordinator = FileLockCoordinator::open(&path).unwrap();
        let positions = Locked::new(&coordinator);
        assert!(positions.record(1, 10));
        assert!(positions.record(2, 20));
        assert!(!positions.header().unwrap().clean);
    }
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert!(coordinator.stored_sequence_header().unwrap().unwrap().clean);
    let positions = Locked::new(&coordinator);
    // Both positions were written in the earlier run.
    assert_eq!(positions.recorded(1), Some((10, false)));
    assert_eq!(positions.recorded(2), Some((20, false)));
    let header = positions.header().unwrap();
    assert!(header.clean && header.run == 2);
    // The first change removes the clean mark, and a position written now belongs to this run.
    assert!(positions.record(1, 10));
    assert!(!positions.header().unwrap().clean);
    assert_eq!(positions.recorded(1), Some((10, true)));
    assert_eq!(positions.recorded(2), Some((20, false)));
}

#[test]
fn positions_changed_since_the_clean_mark_are_discarded_after_an_unclean_stop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unclean.db");
    {
        let coordinator = FileLockCoordinator::open(&path).unwrap();
        assert!(Locked::new(&coordinator).record(1, 10));
    }
    {
        // A process of this test that never detaches stands for one that stops without an orderly close.
        let coordinator = FileLockCoordinator::open(&path).unwrap();
        assert!(Locked::new(&coordinator).record(1, 11));
        std::mem::forget(coordinator);
    }
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let positions = Locked::new(&coordinator);
    assert_eq!(positions.recorded(1), None);
    assert_eq!(positions.header(), None);
    assert!(positions.record(1, 40));
    assert_eq!(positions.recorded(1), Some((40, true)));
}

#[test]
fn retaining_drops_the_positions_of_missing_sequences_and_every_removed_slot() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("retain.db")).unwrap();
    let positions = Locked::new(&coordinator);
    let run = colliding(3);
    for sequence in 0..40 {
        assert!(positions.record(100_000_000 + sequence, i64::from(sequence)));
    }
    for (value, sequence) in (1..).zip(&run) {
        assert!(positions.record(*sequence, value));
    }
    positions.remove(run[0]);
    assert_eq!(positions.header().unwrap().used, 43);
    let kept = [run[1], run[2], 100_000_005, 100_000_006];
    coordinator
        .retain_sequence_positions(&|stored| kept.iter().any(|sequence| key(*sequence) == *stored))
        .unwrap();
    let header = positions.header().unwrap();
    assert_eq!((header.used, header.rebuilding), (4, false));
    assert_eq!(header.crowded_at, CROWDED);
    assert_eq!(coordinator.read_sequence_positions().unwrap().len(), 4);
    assert_eq!(positions.recorded(run[1]), Some((2, true)));
    assert_eq!(positions.recorded(run[2]), Some((3, true)));
    assert_eq!(positions.recorded(100_000_005), Some((5, true)));
    assert_eq!(positions.recorded(100_000_004), None);
    assert_eq!(positions.recorded(run[0]), None);
}

#[test]
fn a_full_table_offers_no_slot_until_missing_sequences_are_dropped() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("full.db")).unwrap();
    let positions = Locked::new(&coordinator);
    assert!(positions.record(1, 10));
    // Every slot holds the position of some other sequence.
    let mut table = vec![0_u8; SLOT_SIZE * SLOT_COUNT as usize];
    for (sequence, slot) in (1_000_000_000..).zip(table.as_chunks_mut::<SLOT_SIZE>().0) {
        slot.copy_from_slice(&encode(&key(sequence), &position(1), 1));
    }
    write_all_at(&coordinator.sequence_file, &table, slot_offset(0)).unwrap();
    coordinator
        .write_sequence_header(&Header {
            used: SLOT_COUNT as u32,
            ..positions.header().unwrap()
        })
        .unwrap();
    let full = positions.read(2);
    assert!(full.recorded.is_none() && !full.recordable && full.crowded);
    assert!(!positions.record(2, 20));
    let stored = home(&key(2));
    coordinator
        .retain_sequence_positions(&|kept| home(kept) != stored && kept.object[3] % 2 == 0)
        .unwrap();
    let header = positions.header().unwrap();
    assert!(header.used < SLOT_COUNT as u32 / 2 + 1);
    let roomy = positions.read(2);
    assert!(roomy.recordable && !roomy.crowded);
    assert!(positions.record(2, 20));
    assert_eq!(positions.recorded(2), Some((20, true)));
}

#[test]
fn a_table_mostly_taken_by_existing_sequences_is_not_rewritten_again() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("taken.db")).unwrap();
    let positions = Locked::new(&coordinator);
    assert!(positions.record(1, 10));
    let mut table = vec![0_u8; SLOT_SIZE * SLOT_COUNT as usize];
    for (sequence, slot) in (1_000_000_000..).zip(table.as_chunks_mut::<SLOT_SIZE>().0) {
        slot.copy_from_slice(&encode(&key(sequence), &position(1), 1));
    }
    write_all_at(&coordinator.sequence_file, &table, slot_offset(0)).unwrap();
    coordinator.retain_sequence_positions(&|_| true).unwrap();
    let header = positions.header().unwrap();
    assert_eq!(header.used, SLOT_COUNT as u32);
    assert_eq!(header.crowded_at, u32::MAX);
    let full = positions.read(2);
    assert!(!full.recordable && !full.crowded);
}

#[test]
fn a_table_a_dead_process_left_half_written_is_discarded() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("torn.db")).unwrap();
    let positions = Locked::new(&coordinator);
    assert!(positions.record(1, 10));
    coordinator
        .write_sequence_header(&Header {
            rebuilding: true,
            ..positions.header().unwrap()
        })
        .unwrap();
    assert_eq!(positions.recorded(1), None);
    assert_eq!(coordinator.sequence_file.metadata().unwrap().len(), 0);
    assert!(positions.record(1, 50));
    assert_eq!(positions.recorded(1), Some((50, true)));
}

#[test]
fn a_file_without_a_header_holds_no_position() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("alien.db")).unwrap();
    let positions = Locked::new(&coordinator);
    let mut table = vec![0xa5_u8; HEADER_SIZE + SLOT_SIZE * 8];
    table[..SLOT_SIZE].fill(0xff);
    write_all_at(&coordinator.sequence_file, &table, 0).unwrap();
    assert_eq!(positions.recorded(1), None);
    assert_eq!(coordinator.read_sequence_positions().unwrap().len(), 0);
    assert!(positions.record(1, 10));
    assert_eq!(positions.recorded(1), Some((10, true)));
    assert_eq!(coordinator.read_sequence_positions().unwrap().len(), 1);
}
