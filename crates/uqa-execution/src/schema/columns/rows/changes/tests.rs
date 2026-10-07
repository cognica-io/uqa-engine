//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::{PendingRowChange, RowLockKey};

const TABLE: [u8; 16] = [3; 16];
const OTHER: [u8; 16] = [7; 16];

#[derive(Default)]
struct Fixture {
    frames: Vec<Vec<TransactionRowChange>>,
    fail: bool,
}

impl RewriteRowChanges for Fixture {
    fn storage_generation(&self, table: &str) -> Result<[u8; 16], SQLError> {
        match table {
            "target" => Ok(TABLE),
            _ => Err(SQLError::UnknownTable(table.into())),
        }
    }

    fn visit_changes(
        &self,
        visit: &mut dyn FnMut(&[TransactionRowChange]) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        for frame in &self.frames {
            visit(frame)?;
        }
        if self.fail {
            return Err(SQLError::Internal("fixture read failed".into()));
        }
        Ok(())
    }
}

fn change(generation: [u8; 16], id: DocId, kind: PendingRowChangeKind) -> TransactionRowChange {
    TransactionRowChange {
        pending: PendingRowChange {
            key: RowLockKey {
                table: 1,
                doc_id: id,
            },
            kind,
        },
        source_generation: generation,
        successor_generation: None,
        query_origin: None,
    }
}

fn rewritten(from: ([u8; 16], DocId), to: Option<([u8; 16], DocId)>) -> TransactionRowChange {
    let mut change = change(
        from.0,
        from.1,
        PendingRowChangeKind::Rewrite(RowLockKey {
            table: 2,
            doc_id: to.map_or(99, |(_, id)| id),
        }),
    );
    change.successor_generation = to.map(|(generation, _)| generation);
    change
}

fn entries(rows: &ChangedRows) -> Vec<(DocId, bool)> {
    let mut result = Vec::new();
    rows.visit(&mut |id, present| {
        result.push((id, present));
        Ok(())
    })
    .unwrap();
    result
}

#[test]
fn surviving_changes_after_the_marker_keep_the_latest_presence() {
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let mut owner = Fixture {
        frames: vec![
            vec![change(TABLE, 2, PendingRowChangeKind::Insert)],
            vec![change(TABLE, 8, PendingRowChangeKind::Delete)],
        ],
        ..Fixture::default()
    };
    let start = marker(&owner).unwrap();
    assert_eq!(start, 2);
    owner.frames[1].extend([
        change(TABLE, 2, PendingRowChangeKind::Update),
        change(TABLE, 2, PendingRowChangeKind::Update),
        change(TABLE, 2, PendingRowChangeKind::Delete),
        change(TABLE, 2, PendingRowChangeKind::Insert),
        change(TABLE, 4, PendingRowChangeKind::Update),
        change(TABLE, 4, PendingRowChangeKind::Delete),
        change(OTHER, 2, PendingRowChangeKind::Delete),
        change(OTHER, 9, PendingRowChangeKind::Insert),
    ]);
    let rows = capture_since(
        &owner,
        start,
        owner.storage_generation("target").unwrap(),
        &memory,
        &cancellation,
    )
    .unwrap();
    assert!(!rows.is_empty());
    assert_eq!(entries(&rows), [(2, true), (4, false)]);
    assert_eq!(rows.get(2).unwrap(), Some(true));
    assert_eq!(rows.get(4).unwrap(), Some(false));
    assert_eq!(rows.get(8).unwrap(), None);
    assert_eq!(rows.get(9).unwrap(), None);
    drop(rows);
    assert_eq!(memory.used(), 0);
}

#[test]
fn rewrites_follow_both_generations_and_keep_full_width_numeric_order() {
    let owner = Fixture {
        frames: vec![vec![
            change(TABLE, 0, PendingRowChangeKind::Insert),
            rewritten((TABLE, DocId::MAX), Some((OTHER, 255))),
            rewritten((OTHER, 0), Some((TABLE, 256))),
            rewritten((TABLE, 1), Some((TABLE, 2))),
            rewritten((TABLE, 9), Some((TABLE, 9))),
            rewritten((TABLE, 333), None),
        ]],
        ..Fixture::default()
    };
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let rows = capture_since(&owner, 0, TABLE, &memory, &cancellation).unwrap();
    assert_eq!(
        entries(&rows),
        [
            (0, true),
            (1, false),
            (2, true),
            (9, true),
            (256, true),
            (333, false),
            (DocId::MAX, false),
        ]
    );
    let other = capture_since(&owner, 0, OTHER, &memory, &cancellation).unwrap();
    assert_eq!(entries(&other), [(0, false), (255, true)]);
    assert_eq!(other.get(DocId::MAX).unwrap(), None);
    let deleted = capture_since(&owner, 5, TABLE, &memory, &cancellation).unwrap();
    assert!(!deleted.is_empty());
    assert_eq!(entries(&deleted), [(333, false)]);
}

#[test]
fn frame_commit_and_rollback_preserve_the_marker_and_retained_views() {
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let mut owner = Fixture {
        frames: vec![
            vec![change(TABLE, 1, PendingRowChangeKind::Insert)],
            vec![change(TABLE, 2, PendingRowChangeKind::Insert)],
        ],
        ..Fixture::default()
    };
    let start = marker(&owner).unwrap();
    owner.frames[1].push(change(TABLE, 3, PendingRowChangeKind::Update));
    owner.frames.push(vec![
        change(TABLE, 4, PendingRowChangeKind::Insert),
        change(TABLE, 3, PendingRowChangeKind::Delete),
    ]);
    let before_rollback = capture_since(&owner, start, TABLE, &memory, &cancellation).unwrap();
    owner.frames.pop();
    let after_rollback = capture_since(&owner, start, TABLE, &memory, &cancellation).unwrap();
    assert_eq!(entries(&before_rollback), [(3, false), (4, true)]);
    assert_eq!(entries(&after_rollback), [(3, true)]);
    let committed = owner.frames.pop().unwrap();
    owner.frames[0].extend(committed);
    let after_commit = capture_since(&owner, start, TABLE, &memory, &cancellation).unwrap();
    assert_eq!(entries(&after_commit), [(3, true)]);
    owner.frames[0].truncate(start);
    let empty = capture_since(&owner, start, TABLE, &memory, &cancellation).unwrap();
    assert!(empty.is_empty());
    owner.frames[0].push(change(TABLE, 5, PendingRowChangeKind::Insert));
    let next = capture_since(&owner, start, TABLE, &memory, &cancellation).unwrap();
    assert_eq!(entries(&next), [(5, true)]);
    assert_eq!(entries(&before_rollback), [(3, false), (4, true)]);
    owner.frames[0].clear();
    assert!(matches!(
        capture_since(&owner, start, TABLE, &memory, &cancellation),
        Err(SQLError::Internal(_))
    ));
}

#[test]
fn an_unchanged_generation_needs_no_record_allocation() {
    let owner = Fixture {
        frames: vec![vec![change(OTHER, 1, PendingRowChangeKind::Insert)]],
        ..Fixture::default()
    };
    let memory = MemoryBudget::new(0);
    let rows = capture_since(&owner, 0, TABLE, &memory, &CancellationToken::new()).unwrap();
    assert!(rows.is_empty());
    assert_eq!(rows.get(1).unwrap(), None);
    assert_eq!(entries(&rows).len(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn spilled_changes_share_the_original_allowance_and_release_replay_resources() {
    verify_spilled_changes(64 << 10, 0, 8_192);
    verify_spilled_changes(256 << 10, 128 << 10, 16_384);
}

fn verify_spilled_changes(limit: usize, caller_bytes: usize, count: u64) {
    let memory = MemoryBudget::new(limit);
    let held = memory.reserve(caller_bytes).unwrap();
    // The keys and one-byte presences alone cannot remain resident beside the caller's reservation.
    assert!(usize::try_from(count).unwrap() * 9 > memory.limit() - held.bytes());
    let owner = Fixture {
        frames: vec![(0..count)
            .rev()
            .map(|id| {
                change(
                    TABLE,
                    id,
                    if id.is_multiple_of(3) {
                        PendingRowChangeKind::Delete
                    } else {
                        PendingRowChangeKind::Update
                    },
                )
            })
            .collect()],
        ..Fixture::default()
    };
    let rows = capture_since(&owner, 0, TABLE, &memory, &CancellationToken::new()).unwrap();
    let retained = memory.used();
    let mut next = 0;
    rows.visit(&mut |id, present| {
        assert_eq!(id, next);
        assert_eq!(present, !id.is_multiple_of(3));
        next += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(next, count);
    assert_eq!(memory.used(), retained);
    assert_eq!(
        rows.get(count - 1).unwrap(),
        Some(!(count - 1).is_multiple_of(3))
    );
    assert_eq!(
        rows.get(count - 2).unwrap(),
        Some(!(count - 2).is_multiple_of(3))
    );
    assert_eq!(rows.get(count).unwrap(), None);
    let mut visited = 0;
    let error = rows
        .visit(&mut |_, _| {
            visited += 1;
            Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: "callback rejected the row".into(),
            })
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    assert_eq!(visited, 1);
    assert_eq!(memory.used(), retained);
    assert!(memory.peak() <= memory.limit());
    drop(rows);
    assert_eq!(memory.used(), held.bytes());
}

#[test]
fn source_errors_and_cancellation_release_partially_retained_changes() {
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let mut owner = Fixture {
        frames: vec![vec![change(TABLE, 1, PendingRowChangeKind::Insert)]],
        fail: true,
    };
    assert!(marker(&owner).is_err());
    let error = capture_since(&owner, 0, TABLE, &memory, &cancellation)
        .err()
        .expect("source error");
    assert!(matches!(error, SQLError::Internal(message) if message == "fixture read failed"));
    assert_eq!(memory.used(), 0);
    owner.fail = false;
    let rows = capture_since(&owner, 0, TABLE, &memory, &cancellation).unwrap();
    cancellation.cancel();
    assert!(matches!(rows.get(1), Err(SQLError::Cancelled(_))));
    assert!(matches!(
        rows.visit(&mut |_, _| Ok(())),
        Err(SQLError::Cancelled(_))
    ));
    drop(rows);
    assert!(matches!(
        capture_since(&owner, 0, TABLE, &memory, &cancellation),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
