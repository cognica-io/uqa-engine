//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runs, tombstones and rebuilds of the claim table file.

use super::*;

const OWNER: Owner = Owner {
    slot: 3,
    generation: 7,
};

fn open(directory: &tempfile::TempDir) -> (File, Header) {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("claims"))
        .unwrap();
    let header = initialize(&file, None).unwrap();
    (file, header)
}

fn row(identity: u64) -> super::super::RowIdentity {
    super::super::RowIdentity::Relation {
        generation: 1,
        doc_id: identity,
    }
}

fn entry(identity: u64) -> Entry {
    Entry {
        identity: row(identity),
        session: identity + 1,
        owner: OWNER,
        key: Mode::Shared,
        row: Mode::Exclusive,
    }
}

/// The first `count` identities whose home slot in a table of `capacity_log2` is `slot`.
fn identities_at(slot: u64, capacity_log2: u32, count: usize) -> Vec<u64> {
    (1_u64..)
        .filter(|identity| home(row(*identity), capacity_log2) == slot)
        .take(count)
        .collect()
}

/// Where `identity` is stored, with the slots its run occupies.
fn find(table: &Table<'_>, identity: u64) -> (Option<u64>, Probe) {
    let mut found = None;
    let probe = table
        .probe(row(identity), &mut |index, slot| {
            if matches!(slot, Slot::Live(entry) if entry.identity == row(identity)) {
                found = Some(index);
            }
            Ok(())
        })
        .unwrap();
    (found, probe)
}

fn insert(table: &Table<'_>, identity: u64) -> u64 {
    let (_, probe) = find(table, identity);
    let index = probe.empty.expect("a free slot");
    table.write(index, &Slot::Live(entry(identity))).unwrap();
    index
}

fn live(table: &Table<'_>) -> Vec<Entry> {
    let mut entries = Vec::new();
    table
        .scan(&mut |entry| {
            entries.push(entry);
            Ok(())
        })
        .unwrap();
    entries
}

#[test]
fn an_entry_and_a_header_survive_their_encoding() {
    let stored = Slot::Live(Entry {
        identity: row(u64::MAX),
        session: u64::MAX,
        owner: Owner {
            slot: 4095,
            generation: u32::MAX,
        },
        key: Mode::None,
        row: Mode::Shared,
    });
    assert_eq!(Slot::decode(&stored.encode()).unwrap(), stored);
    assert_eq!(
        Slot::decode(&Slot::Tombstone.encode()).unwrap(),
        Slot::Tombstone
    );
    let mut interrupted = [0xff; ENTRY_SIZE as usize];
    interrupted[STATE_OFFSET] = STATE_TOMBSTONE;
    assert_eq!(Slot::decode(&interrupted).unwrap(), Slot::Tombstone);
    assert_eq!(
        Slot::decode(&[0; ENTRY_SIZE as usize]).unwrap(),
        Slot::Empty
    );
    let mut without_mode = stored.encode();
    without_mode[64] = 0;
    assert!(Slot::decode(&without_mode).is_err());

    let header = Header {
        capacity_log2: 17,
        epoch: 9,
        rebuild: Some(Rebuild {
            journal: 1 << 30,
            entries: 12_345,
            capacity_log2: 18,
        }),
    };
    assert_eq!(Header::decode(&header.encode()).unwrap(), header);
    let mut unknown = header.encode();
    unknown[7] = 3;
    assert!(Header::decode(&unknown).is_err());
}

#[test]
fn a_new_sidecar_has_no_header_and_an_initialized_one_is_empty() {
    let directory = tempfile::tempdir().unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("claims"))
        .unwrap();
    assert_eq!(read_header(&file).unwrap(), None);
    assert!(read_processes(&file)
        .unwrap()
        .iter()
        .all(|record| *record == (0, 0)));
    write_process(&file, 5, 2, 4242).unwrap();
    let header = initialize(&file, None).unwrap();
    assert_eq!(read_header(&file).unwrap(), Some(header));
    assert_eq!(
        file.metadata().unwrap().len(),
        slot_offset(slots(INITIAL_CAPACITY_LOG2))
    );
    assert_eq!(live(&Table::new(&file, &header)).len(), 0);
    assert_eq!(read_process(&file, 5).unwrap(), (2, 4242));
    assert_eq!(
        initialize(&file, Some(header)).unwrap().epoch,
        header.epoch + 1
    );
    assert_eq!(read_process(&file, 5).unwrap(), (2, 4242));
}

#[test]
fn a_run_empties_once_every_claim_in_it_is_removed() {
    let directory = tempfile::tempdir().unwrap();
    let (file, header) = open(&directory);
    let table = Table::new(&file, &header);
    let identities = identities_at(100, header.capacity_log2, 5);
    let slots = identities
        .iter()
        .map(|identity| insert(&table, *identity))
        .collect::<Vec<_>>();
    assert_eq!(slots, [100, 101, 102, 103, 104]);

    // A claim inside the run leaves a tombstone, so the claims after it stay reachable.
    table.remove(101).unwrap();
    table.remove(103).unwrap();
    for (identity, slot) in [
        (identities[0], 100),
        (identities[2], 102),
        (identities[4], 104),
    ] {
        assert_eq!(find(&table, identity).0, Some(slot));
    }
    assert_eq!(find(&table, identities[1]).0, None);
    assert_eq!(find(&table, identities[4]).1.length, 5);

    // The claim that ends the run takes the tombstones before it along.
    table.remove(104).unwrap();
    assert_eq!(find(&table, identities[2]).1.length, 3);
    assert_eq!(find(&table, identities[2]).1.empty, Some(103));
    table.remove(100).unwrap();
    assert_eq!(find(&table, identities[2]).0, Some(102));
    table.remove(102).unwrap();
    let probe = find(&table, identities[2]).1;
    assert_eq!((probe.empty, probe.length), (Some(100), 0));
    assert_eq!(live(&table).len(), 0);
}

#[test]
fn a_tombstone_is_reused_without_hiding_the_claims_after_it() {
    let directory = tempfile::tempdir().unwrap();
    let (file, header) = open(&directory);
    let table = Table::new(&file, &header);
    let identities = identities_at(7, header.capacity_log2, 4);
    for identity in &identities[..3] {
        insert(&table, *identity);
    }
    table.remove(8).unwrap();
    let mut tombstone = None;
    table
        .probe(row(identities[3]), &mut |index, slot| {
            if slot == Slot::Tombstone {
                tombstone.get_or_insert(index);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(tombstone, Some(8));
    table.write(8, &Slot::Live(entry(identities[3]))).unwrap();
    assert_eq!(find(&table, identities[3]).0, Some(8));
    assert_eq!(find(&table, identities[2]).0, Some(9));
}

#[test]
fn a_run_that_reaches_the_end_of_the_table_has_no_free_slot() {
    let directory = tempfile::tempdir().unwrap();
    let (file, mut header) = open(&directory);
    let last = header.capacity() - 1;
    let identities = identities_at(last, header.capacity_log2, MARGIN as usize + 2);
    {
        let table = Table::new(&file, &header);
        for identity in &identities[..=MARGIN as usize] {
            insert(&table, *identity);
        }
        let probe = find(&table, identities[MARGIN as usize + 1]).1;
        assert_eq!((probe.empty, probe.length), (None, MARGIN + 1));
    }

    // The same claims cannot be placed at this capacity with one more, so the rebuild grows.
    let mut entries = live(&Table::new(&file, &header));
    entries.push(entry(identities[MARGIN as usize + 1]));
    rebuild(&file, &mut header, entries, 0).unwrap();
    assert!(header.capacity_log2 > INITIAL_CAPACITY_LOG2);
    let table = Table::new(&file, &header);
    for identity in identities {
        assert!(find(&table, identity).0.is_some());
    }
}

#[test]
fn a_rebuild_keeps_exactly_its_entries_at_a_quarter_full() {
    let directory = tempfile::tempdir().unwrap();
    let (file, mut header) = open(&directory);
    let kept = {
        let table = Table::new(&file, &header);
        for identity in 1..=1500 {
            insert(&table, identity);
        }
        for identity in 1..=1500_u64 {
            if identity % 3 == 0 {
                table.remove(find(&table, identity).0.unwrap()).unwrap();
            }
        }
        live(&table)
    };
    assert_eq!(kept.len(), 1000);
    let epoch = header.epoch;
    rebuild(&file, &mut header, kept, 3000).unwrap();
    assert_eq!(header.capacity(), 16_384);
    assert_eq!((header.epoch, header.rebuild), (epoch + 1, None));
    assert_eq!(read_header(&file).unwrap(), Some(header));
    assert_eq!(
        file.metadata().unwrap().len(),
        slot_offset(slots(header.capacity_log2))
    );
    let table = Table::new(&file, &header);
    for identity in 1..=1500_u64 {
        let (found, probe) = find(&table, identity);
        assert_eq!(found.is_some(), identity % 3 != 0, "{identity}");
        assert!(probe.empty.is_some());
    }
    let mut tombstones = 0;
    for identity in 1..=1500 {
        table
            .probe(row(identity), &mut |_, slot| {
                tombstones += u64::from(slot == Slot::Tombstone);
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(tombstones, 0);

    // A table that holds little shrinks again.
    rebuild(&file, &mut header, vec![entry(77)], 0).unwrap();
    assert_eq!(header.capacity_log2, INITIAL_CAPACITY_LOG2);
    assert_eq!(live(&Table::new(&file, &header)), [entry(77)]);
}

#[test]
fn a_rebuild_interrupted_after_its_journal_is_finished_by_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let (file, mut header) = open(&directory);
    let mut entries = {
        let table = Table::new(&file, &header);
        for identity in 1..=3000 {
            insert(&table, identity);
        }
        live(&table)
    };
    journal(&file, &mut header, &mut entries, 0).unwrap();
    assert!(header.rebuild.is_some());
    // The dead process had overwritten part of the table when it stopped.
    write_all_at(&file, &vec![0xa5; 64 * 1024], slot_offset(10)).unwrap();

    let mut recovered = read_header(&file).unwrap().unwrap();
    assert_eq!(recovered, header);
    recover(&file, &mut recovered).unwrap();
    assert_eq!(recovered.rebuild, None);
    assert_eq!(read_header(&file).unwrap(), Some(recovered));
    let table = Table::new(&file, &recovered);
    for identity in 1..=3000 {
        assert!(find(&table, identity).0.is_some(), "{identity}");
    }
    assert_eq!(live(&table).len(), 3000);
    assert_eq!(
        file.metadata().unwrap().len(),
        slot_offset(slots(recovered.capacity_log2))
    );

    // A journal that was never named leaves the table as it was.
    let mut unchanged = recovered;
    recover(&file, &mut unchanged).unwrap();
    assert_eq!(unchanged, recovered);
}
