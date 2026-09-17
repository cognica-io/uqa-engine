//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence catalog operations share the caller's chosen session and commit boundary.

use std::sync::Arc;

use crate::{
    CatalogFacade, KeyValueCatalog, KeyValueStore, RelationIdentity, SequenceOptions,
    SequenceReservationResult, SequenceRow, SequenceValueReservation, StorageBackendResult,
};

fn sequence(name: &str, id: u8) -> SequenceRow {
    SequenceRow {
        relation: RelationIdentity::new("public", name),
        role_owner: "owner".into(),
        acl: None,
        object_id: [id; 16],
        definition_generation: [id + 64; 16],
        start: 1,
        increment: 1,
        current: 1,
        called: false,
        log_count: 0,
        persistence: "p".into(),
        owner: None,
        options: SequenceOptions {
            min_value: Some(1),
            max_value: Some(i64::MAX),
            cache_size: 3,
            ..SequenceOptions::default()
        },
    }
}

fn reserve(
    catalog: &KeyValueCatalog,
    row: &SequenceRow,
) -> StorageBackendResult<SequenceValueReservation> {
    let SequenceReservationResult::Reserved(reservation) = catalog.reserve_sequence_values(
        &row.relation.qualified_name(),
        row.object_id,
        row.definition_generation,
    )?
    else {
        panic!("expected a sequence reservation");
    };
    Ok(reservation)
}

/// Exercise independent sequence consumers, selected autonomous sessions, definition conflicts and undo; leave `public.sequence_saved` for reopen verification. Use fresh disposable storage.
pub fn verify_sequence_concurrency(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let first = KeyValueCatalog::new(a.clone());
    let second = KeyValueCatalog::new(b.clone());
    first.save_schema("public")?;
    let row = sequence("sequence_a", 1);
    let independent = sequence("sequence_b", 2);
    assert!(first.create_sequence_row(&row)?);
    assert!(first.create_sequence_row(&independent)?);
    a.begin_transaction()?;
    first.save_model("sequence_private", "kept")?;
    assert_eq!(reserve(&first, &row)?.first_value, 1);
    a.savepoint("sequence_position")?;
    first.set_sequence_value("sequence_a", row.object_id, 40, false, 0)?;
    assert_eq!(reserve(&first, &row)?.last_value, 42);
    b.begin_transaction()?;
    assert_eq!(reserve(&second, &independent)?.last_value, 3);
    b.commit_transaction()?;
    a.rollback_to_savepoint("sequence_position")?;
    a.release_savepoint("sequence_position")?;
    a.commit_transaction()?;
    assert_eq!(reserve(&first, &row)?.first_value, 4);
    assert_eq!(reserve(&first, &independent)?.first_value, 4);

    // The catalog obeys its selected session; execution chooses the autonomous sibling for committed SQL sequences.
    a.begin_transaction()?;
    first.save_model("sequence_private", "discarded")?;
    assert_eq!(reserve(&second, &row)?.first_value, 7);
    a.rollback_transaction()?;
    assert_eq!(
        first.load_model("sequence_private")?.as_deref(),
        Some("kept")
    );
    assert_eq!(reserve(&first, &row)?.first_value, 10);
    first.drop_sequence_row("sequence_a")?;
    first.drop_sequence_row("sequence_b")?;

    verify_sequence_conflicts(a, b, &first, &second)?;

    let row = sequence("sequence_saved", 6);
    first.create_sequence_row(&row)?;
    a.begin_transaction()?;
    a.savepoint("sequence_definition")?;
    let mut changed = row.clone();
    changed.definition_generation = [100; 16];
    changed.current = 100;
    first.replace_sequence_row(&changed)?;
    assert_eq!(reserve(&first, &changed)?.first_value, 100);
    first.rename_sequence_row("sequence_saved", "sequence_temporary")?;
    a.rollback_to_savepoint("sequence_definition")?;
    a.release_savepoint("sequence_definition")?;
    assert_eq!(first.load_sequence_rows()?, std::slice::from_ref(&row));
    assert_eq!(reserve(&first, &row)?.last_value, 3);
    first.rename_sequence_row("sequence_saved", "sequence_temporary")?;
    first.rename_sequence_row("sequence_temporary", "sequence_saved")?;
    a.commit_transaction()?;
    verify_sequence_reopen(a)
}

fn verify_sequence_conflicts(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
    first: &KeyValueCatalog,
    second: &KeyValueCatalog,
) -> StorageBackendResult<()> {
    for change in ["reserve", "set", "replace", "rename", "drop"] {
        for change_wins in [false, true] {
            let row = sequence("sequence_conflict", 3);
            assert!(first.create_sequence_row(&row)?);
            a.begin_transaction()?;
            b.begin_transaction()?;
            assert_eq!(reserve(first, &row)?.first_value, 1);
            match change {
                "reserve" => assert_eq!(reserve(second, &row)?.first_value, 1),
                "set" => assert_eq!(
                    second.set_sequence_value("sequence_conflict", row.object_id, 100, false, 0)?,
                    Some(100)
                ),
                "replace" => {
                    let mut changed = row.clone();
                    changed.definition_generation = [99; 16];
                    changed.current = 100;
                    assert!(second.replace_sequence_row(&changed)?);
                }
                "rename" => {
                    assert!(second.rename_sequence_row("sequence_conflict", "sequence_moved")?);
                }
                _ => assert!(second.drop_sequence_row("sequence_conflict")?),
            }
            let (winner, loser, catalog) = if change_wins {
                (b, a, second)
            } else {
                (a, b, first)
            };
            winner.commit_transaction()?;
            let expected = catalog.load_sequence_rows()?;
            let error = loser
                .commit_transaction()
                .expect_err("overlapping sequence writes must conflict");
            let crate::StorageBackendError::Backend { source, .. } = error else {
                panic!("expected a sequence write conflict: {error}");
            };
            assert!(
                matches!(
                    source.downcast_ref::<crate::mvcc::VersionError>(),
                    Some(crate::mvcc::VersionError::WriteConflict { .. })
                ),
                "{source}"
            );
            loser.rollback_transaction()?;
            assert_eq!(first.load_sequence_rows()?, expected);
            assert_eq!(second.load_sequence_rows()?, expected);
            first.drop_sequence_row("sequence_conflict")?;
            first.drop_sequence_row("sequence_moved")?;
        }
    }

    for other_wins in [false, true] {
        let first_row = sequence("sequence_claim", 4);
        let second_row = sequence("sequence_claim", 5);
        a.begin_transaction()?;
        b.begin_transaction()?;
        assert!(first.create_sequence_row(&first_row)?);
        assert!(second.create_sequence_row(&second_row)?);
        let (winner, loser, expected) = if other_wins {
            (b, a, &second_row)
        } else {
            (a, b, &first_row)
        };
        winner.commit_transaction()?;
        assert!(loser.commit_transaction().is_err());
        loser.rollback_transaction()?;
        assert_eq!(first.load_sequence_rows()?, std::slice::from_ref(expected));
        first.drop_sequence_row("sequence_claim")?;
    }

    Ok(())
}

/// Verify the committed position, object identity and released rename destination left by `verify_sequence_concurrency` after closing all original handles.
pub fn verify_sequence_reopen(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    let mut expected = sequence("sequence_saved", 6);
    expected.current = 3;
    expected.called = true;
    expected.log_count = 32;
    assert_eq!(catalog.load_sequence_rows()?, [expected]);
    let probe = sequence("sequence_temporary", 7);
    assert!(catalog.create_sequence_row(&probe)?);
    assert!(catalog.drop_sequence_row("sequence_temporary")?);
    Ok(())
}
