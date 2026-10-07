//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row claims of one process and of two.

use uqa_sql::ast::LockStrength;

use super::super::super::row_byte_claims;
use super::*;

mod peer;

const RELATION: &[u8] = b"public.row_claim_test";
const PARENT_SESSION: u64 = 101;
const PEER_SESSION: u64 = 202;
const STRENGTHS: [LockStrength; 4] = [
    LockStrength::ForKeyShare,
    LockStrength::ForShare,
    LockStrength::ForNoKeyUpdate,
    LockStrength::ForUpdate,
];

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn warm_row_claims_do_not_issue_positioned_io_per_row() {
    for fallback in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = FileLockCoordinator::open(&directory.path().join("database")).unwrap();
        if fallback {
            coordinator.claim_mapping.lock().disable();
        }
        claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForUpdate);
        let mapped = coordinator.claim_mapping.lock().active();
        table::take_positioned_io();
        for id in 2..=65 {
            claim(&coordinator, PARENT_SESSION, id, LockStrength::ForUpdate);
        }
        let operations = table::take_positioned_io();
        eprintln!("64 claims: mapped={mapped}, positioned reads/writes={operations:?}");
        assert_eq!(operations, if mapped { (0, 0) } else { (128, 64) });
        assert_eq!(stored(&coordinator).1.len(), 65);
    }
}

#[test]
fn a_truncated_claim_file_returns_an_error_without_using_the_old_extent() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("truncated.db")).unwrap();
    claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForUpdate);
    {
        let mut state = coordinator.state.lock();
        let _lock = coordinator.lock_row_claim_table(&mut state.rows).unwrap();
        // Simulate a damaged sidecar between operations, retaining its header.
        coordinator.claim_file.set_len(64).unwrap();
    }
    assert!(coordinator
        .try_claim(PARENT_SESSION, &claims(2, LockStrength::ForUpdate))
        .is_err());
}

fn claims(doc_id: u64, strength: LockStrength) -> Vec<ByteClaim> {
    row_byte_claims(RELATION, doc_id, strength)
}

fn claim(coordinator: &FileLockCoordinator, session: u64, doc_id: u64, strength: LockStrength) {
    assert_eq!(
        coordinator.try_claim(session, &claims(doc_id, strength)),
        Ok(Ok(())),
        "{doc_id} {strength:?}"
    );
}

/// Every entry of the claim table with the header it was read under.
fn stored(coordinator: &FileLockCoordinator) -> (Header, Vec<Entry>) {
    let mut state = coordinator.state.lock();
    let lock = coordinator.lock_row_claim_table(&mut state.rows).unwrap();
    let mut entries = Vec::new();
    lock.table()
        .scan(&mut |entry| {
            entries.push(entry);
            Ok(())
        })
        .unwrap();
    (lock.header, entries)
}

fn modes(coordinator: &FileLockCoordinator) -> Vec<(u64, Mode, Mode)> {
    let mut modes = stored(coordinator)
        .1
        .into_iter()
        .map(|entry| (entry.session, entry.key, entry.row))
        .collect::<Vec<_>>();
    modes.sort_unstable();
    modes
}

#[test]
fn liveness_bytes_fit_every_supported_offset_width() {
    use crate::row_locks::cross_process::{row_span_for_offset_width, RELATION_SPAN, ROW_BASE};
    for (width, maximum) in [(4, i32::MAX as u64), (8, i64::MAX as u64)] {
        let liveness = ROW_BASE + 2 * row_span_for_offset_width(width) + 16 * RELATION_SPAN;
        assert!(liveness + u64::from(table::PROCESS_SLOT_COUNT) <= maximum);
    }
    assert_eq!(
        PROCESS_LIVENESS_BASE,
        ROW_BASE
            + 2 * row_span_for_offset_width(std::mem::size_of::<libc_offset::Width>())
            + 16 * RELATION_SPAN
    );
}

mod libc_offset {
    #[cfg(unix)]
    pub(super) type Width = libc::off_t;
    #[cfg(not(unix))]
    pub(super) type Width = i64;
}

#[test]
fn row_claims_address_a_row_byte_and_never_a_record_lock() {
    for (strength, expected) in STRENGTHS.into_iter().zip([
        vec![(RowByte::Key, false)],
        vec![(RowByte::Row, false)],
        vec![(RowByte::Row, true)],
        vec![(RowByte::Key, true), (RowByte::Row, true)],
    ]) {
        let claims = claims(42, strength);
        let identity = row_claim_address(claims[0]).unwrap().0;
        assert_eq!(
            claims
                .iter()
                .map(|claim| {
                    let (row, byte) = row_claim_address(*claim).unwrap();
                    assert_eq!(row, identity);
                    assert_eq!(row_claim(row, byte, claim.write), *claim);
                    (byte, claim.write)
                })
                .collect::<Vec<_>>(),
            expected
        );
    }
    assert_ne!(
        row_claim_address(claims(42, LockStrength::ForUpdate)[0]),
        row_claim_address(claims(43, LockStrength::ForUpdate)[0])
    );
    assert_ne!(
        row_claim_address(claims(42, LockStrength::ForUpdate)[0]),
        row_claim_address(row_byte_claims(b"public.other", 42, LockStrength::ForUpdate)[0])
    );
    assert_eq!(
        row_claim_address(ByteClaim {
            offset: 10_000,
            write: true
        }),
        None
    );
}

#[test]
fn a_session_keeps_one_entry_with_its_strongest_modes() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("modes.db")).unwrap();
    claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForKeyShare);
    assert_eq!(
        modes(&coordinator),
        [(PARENT_SESSION, Mode::Shared, Mode::None)]
    );
    claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForShare);
    claim(
        &coordinator,
        PARENT_SESSION,
        1,
        LockStrength::ForNoKeyUpdate,
    );
    assert_eq!(
        modes(&coordinator),
        [(PARENT_SESSION, Mode::Shared, Mode::Exclusive)]
    );
    claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForUpdate);
    assert_eq!(
        modes(&coordinator),
        [(PARENT_SESSION, Mode::Exclusive, Mode::Exclusive)]
    );

    // Each acquisition is released on its own, and the entry follows the strongest one left.
    coordinator.release(PARENT_SESSION, &claims(1, LockStrength::ForUpdate));
    assert_eq!(
        modes(&coordinator),
        [(PARENT_SESSION, Mode::Shared, Mode::Exclusive)]
    );
    coordinator.release(PARENT_SESSION, &claims(1, LockStrength::ForNoKeyUpdate));
    assert_eq!(
        modes(&coordinator),
        [(PARENT_SESSION, Mode::Shared, Mode::Shared)]
    );
    coordinator.release(PARENT_SESSION, &claims(1, LockStrength::ForShare));
    coordinator.release(PARENT_SESSION, &claims(1, LockStrength::ForKeyShare));
    assert!(modes(&coordinator).is_empty());
    let state = coordinator.state.lock();
    assert!(state.rows.identities.is_empty());
    assert_eq!(state.rows.entries, 0);
    // Row claims take no record lock and no holder slot.
    assert!(state.claims.is_empty() && state.holders.is_empty());
    assert!(state.pending_holders.is_empty() && state.pending_order.is_empty());
}

#[test]
fn a_claim_made_twice_is_released_twice() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("twice.db")).unwrap();
    claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForShare);
    claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForShare);
    coordinator.release(PARENT_SESSION, &claims(1, LockStrength::ForShare));
    assert_eq!(
        modes(&coordinator),
        [(PARENT_SESSION, Mode::None, Mode::Shared)]
    );
    coordinator.release(PARENT_SESSION, &claims(1, LockStrength::ForShare));
    assert!(modes(&coordinator).is_empty());
    // Releasing a claim that is not held changes nothing.
    coordinator.release(PARENT_SESSION, &claims(1, LockStrength::ForShare));
    coordinator.release(PARENT_SESSION, &claims(2, LockStrength::ForUpdate));
    assert!(modes(&coordinator).is_empty());
}

#[test]
fn sessions_of_one_process_keep_their_own_entries() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("sessions.db")).unwrap();
    claim(&coordinator, 17, 1, LockStrength::ForShare);
    claim(&coordinator, 23, 1, LockStrength::ForShare);
    claim(&coordinator, 23, 1, LockStrength::ForKeyShare);
    assert_eq!(
        modes(&coordinator),
        [
            (17, Mode::None, Mode::Shared),
            (23, Mode::Shared, Mode::Shared)
        ]
    );
    let wanted = claims(1, LockStrength::ForUpdate);
    let state = coordinator.state.lock();
    assert_eq!(state.rows.holders(wanted[0]), [23]);
    let mut row = state.rows.holders(wanted[1]);
    row.sort_unstable();
    assert_eq!(row, [17, 23]);
    assert!(state
        .rows
        .holders(claims(1, LockStrength::ForShare)[0])
        .is_empty());
    assert!(state
        .rows
        .holders(claims(2, LockStrength::ForUpdate)[0])
        .is_empty());
    drop(state);
    // The sessions of this process are not foreign holders.
    assert!(coordinator.foreign_row_holders(wanted[1]).is_empty());
    coordinator.release(17, &claims(1, LockStrength::ForShare));
    assert_eq!(modes(&coordinator), [(23, Mode::Shared, Mode::Shared)]);
}

#[test]
fn a_bulk_claim_grows_the_table_and_its_release_returns_it() {
    const ROWS: u64 = 20_000;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bulk.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    for doc_id in 1..=ROWS {
        claim(
            &coordinator,
            PARENT_SESSION,
            doc_id,
            LockStrength::ForUpdate,
        );
    }
    let (header, entries) = stored(&coordinator);
    assert_eq!(entries.len() as u64, ROWS);
    assert!(header.capacity() >= 2 * ROWS, "{header:?}");
    assert!(entries
        .iter()
        .all(|entry| (entry.key, entry.row) == (Mode::Exclusive, Mode::Exclusive)));
    assert_eq!(coordinator.state.lock().rows.entries, ROWS);

    // One release of every claim, as a commit makes, empties the table and gives its space back.
    let all = (1..=ROWS)
        .flat_map(|doc_id| claims(doc_id, LockStrength::ForUpdate))
        .collect::<Vec<_>>();
    coordinator.release(PARENT_SESSION, &all);
    let (released, entries) = stored(&coordinator);
    assert!(entries.is_empty());
    assert_eq!(released.capacity_log2, table::INITIAL_CAPACITY_LOG2);
    assert!(released.epoch > header.epoch);
    let state = coordinator.state.lock();
    assert!(state.rows.identities.is_empty());
    assert_eq!(state.rows.entries, 0);
    drop(state);
    let mut sidecar = path.into_os_string();
    sidecar.push(".uqa-row-claims");
    assert!(std::fs::metadata(sidecar).unwrap().len() < 256 * 1024);
}

#[test]
fn entries_of_a_dead_process_do_not_block_and_are_removed() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("dead.db")).unwrap();
    claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForKeyShare);
    let wanted = claims(2, LockStrength::ForUpdate);
    let identity = row_claim_address(wanted[0]).unwrap().0;
    {
        // A process that held row 2 and died: its slot names it, and no process holds its liveness byte.
        let mut state = coordinator.state.lock();
        let lock = coordinator.lock_row_claim_table(&mut state.rows).unwrap();
        table::write_process(&coordinator.claim_file, 9, 3, 4_000_000).unwrap();
        let mut empty = None;
        let probe = lock.table().probe(identity, &mut |_, _| Ok(())).unwrap();
        empty.get_or_insert(probe.empty.unwrap());
        lock.table()
            .write(
                empty.unwrap(),
                &Slot::Live(Entry {
                    identity,
                    session: 77,
                    owner: Owner {
                        slot: 9,
                        generation: 3,
                    },
                    key: Mode::Exclusive,
                    row: Mode::Exclusive,
                }),
            )
            .unwrap();
    }
    assert_eq!(stored(&coordinator).1.len(), 2);
    assert!(coordinator.foreign_row_holders(wanted[1]).is_empty());
    assert_eq!(coordinator.try_claim(PARENT_SESSION, &wanted), Ok(Ok(())));
    let entries = stored(&coordinator).1;
    assert_eq!(entries.len(), 2);
    assert!(entries
        .iter()
        .all(|entry| entry.session == PARENT_SESSION && entry.owner.slot == 0));

    // An entry of an earlier attachment of a slot is dead even while the slot has a live owner.
    let stale = claims(3, LockStrength::ForUpdate);
    let identity = row_claim_address(stale[0]).unwrap().0;
    {
        let mut state = coordinator.state.lock();
        let own = state.rows.owner.unwrap();
        let lock = coordinator.lock_row_claim_table(&mut state.rows).unwrap();
        let probe = lock.table().probe(identity, &mut |_, _| Ok(())).unwrap();
        lock.table()
            .write(
                probe.empty.unwrap(),
                &Slot::Live(Entry {
                    identity,
                    session: PARENT_SESSION,
                    owner: Owner {
                        slot: own.slot,
                        generation: own.generation.wrapping_sub(1),
                    },
                    key: Mode::Exclusive,
                    row: Mode::Exclusive,
                }),
            )
            .unwrap();
    }
    assert_eq!(coordinator.try_claim(PARENT_SESSION, &stale), Ok(Ok(())));
    assert_eq!(stored(&coordinator).1.len(), 3);
}

#[test]
fn a_process_attaching_alone_starts_from_an_empty_table() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("alone.db");
    let first = {
        let coordinator = FileLockCoordinator::open(&path).unwrap();
        for doc_id in 1..=3000 {
            claim(
                &coordinator,
                PARENT_SESSION,
                doc_id,
                LockStrength::ForUpdate,
            );
        }
        let (header, entries) = stored(&coordinator);
        assert_eq!(entries.len(), 3000);
        assert!(header.capacity_log2 > table::INITIAL_CAPACITY_LOG2);
        let owner = coordinator.state.lock().rows.owner.unwrap();
        (header, owner)
    };
    // The coordinator closed with its claims in the table, as a process that exits inside a transaction leaves them.
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let (header, entries) = stored(&coordinator);
    assert!(entries.is_empty());
    assert_eq!(header.capacity_log2, table::INITIAL_CAPACITY_LOG2);
    assert!(header.epoch > first.0.epoch);
    let owner = coordinator.state.lock().rows.owner.unwrap();
    assert_eq!(owner.slot, first.1.slot);
    assert_eq!(owner.generation, first.1.generation + 1);
    claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForUpdate);
    assert_eq!(stored(&coordinator).1.len(), 1);
}
