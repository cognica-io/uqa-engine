//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Moving the value state of sequence definitions into value records gives every retained revision of a definition a value record with the value state it held, atomically with the format change.

use super::*;
use crate::mvcc::native::decode_record;
use crate::mvcc::native::tests::materialization::{records, replace, with};
use rusqlite::types::Value;
use uqa_storage::mvcc::{CommitSequence, PreparedRecordCommit, RecordWrite, VersionedPersistence};

/// The version rows of every record of `family`, in key and commit order.
fn versions(
    connection: &ManagedConnection,
    family: Family,
) -> Vec<(Vec<u8>, u64, Option<Vec<u8>>)> {
    let control = StorageReadControl::with_limit(1 << 22);
    let prefix = super::super::super::NativeRecordIdentity::family_prefix(family, &control)
        .unwrap()
        .to_vec();
    with(connection, |sqlite| {
        let mut statement = sqlite.prepare(
            "SELECT key, sequence, value FROM _uqa_mvcc_versions WHERE substr(key, 1, ?2) = ?1 ORDER BY key, sequence",
        )?;
        let rows = statement
            .query_map(
                rusqlite::params![prefix, i64::try_from(prefix.len()).unwrap()],
                |row| {
                    let sequence: Vec<u8> = row.get(1)?;
                    Ok((
                        row.get(0)?,
                        u64::from_be_bytes(sequence.try_into().expect("commit sequence")),
                        row.get(2)?,
                    ))
                },
            )?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    })
}

/// The value state a value record version holds, or `None` for a deletion.
fn value_state(key: &[u8], value: Option<&[u8]>) -> Option<(i64, i64, i64)> {
    let control = StorageReadControl::with_limit(1 << 22);
    value.map(|bytes| {
        let (_, row) = decode_record(key, bytes, &control).unwrap();
        let integer = |value: rusqlite::types::ValueRef<'_>| value.as_i64().unwrap();
        (integer(row[2]), integer(row[3]), integer(row[4]))
    })
}

/// A definition revision with the value columns of an earlier format, which moved the value state in the definition.
fn with_value_state(record: &NativeRecord, state: (i64, i64, i64)) -> NativeRecord {
    let control = StorageReadControl::with_limit(1 << 22);
    let record = replace(
        record,
        5,
        rusqlite::types::ValueRef::Integer(state.0),
        &control,
    );
    let record = replace(
        &record,
        6,
        rusqlite::types::ValueRef::Integer(state.1),
        &control,
    );
    replace(
        &record,
        20,
        rusqlite::types::ValueRef::Integer(state.2),
        &control,
    )
}

/// Strip the value records and their table from a database, and mark it with the previous format, which had neither.
fn downgrade(connection: &ManagedConnection, namespace: uqa_storage::mvcc::DatabaseId) {
    let control = StorageReadControl::with_limit(1 << 22);
    let prefix =
        super::super::super::NativeRecordIdentity::family_prefix(Family::SequenceValues, &control)
            .unwrap()
            .to_vec();
    with(connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        for table in ["_uqa_mvcc_versions", "_uqa_mvcc_heads"] {
            transaction.execute(
                &format!("DELETE FROM {table} WHERE substr(key, 1, ?2) = ?1"),
                rusqlite::params![prefix, i64::try_from(prefix.len()).unwrap()],
            )?;
        }
        transaction.execute_batch(&format!(
            "DROP TABLE {}; DROP TABLE _uqa_mvcc_native_format",
            Family::SequenceValues.layout().table
        ))?;
        transaction.execute_batch(FORMAT_THIRTEEN)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES(1,13,49,?1)",
            [namespace.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 13)?;
        Ok(())
    });
}

/// A database of the previous format with two sequences: `kept`, whose definition moved its value state in two later revisions, and `dropped`, whose definition was deleted by the second.
fn previous_format_history() -> (
    ManagedConnection,
    uqa_storage::mvcc::DatabaseId,
    NativeRecord,
    NativeRecord,
) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    with(&connection, |sqlite| {
        sqlite.execute_batch("INSERT INTO _relations VALUES ('public', 'kept', 'sequence'), ('public', 'dropped', 'sequence'); INSERT INTO _sequences(schema_name, relation_name, start, increment, current, called, log_count) VALUES ('public', 'kept', 1, 1, 1, 0, 0), ('public', 'dropped', 1, 1, 1, 0, 0);")?;
        Ok(())
    });
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let namespace = store.native_namespace().unwrap();
    let definitions = records(&connection, &store, Family::Sequences, &control);
    let values = records(&connection, &store, Family::SequenceValues, &control);
    let owner = |record: &NativeRecord| {
        decode_record(record.key(), record.row(), &control)
            .unwrap()
            .0
            .owner()
    };
    // Physical order is by name: `dropped` precedes `kept`.
    let (dropped, kept) = (&definitions[0], &definitions[1]);
    // Revisions as the previous format wrote them, which moved the value state in the definition.
    let logged = with_value_state(kept, (33, 1, 0));
    let id = store.allocate_transaction(&control).unwrap();
    store
        .commit(
            id,
            &PreparedRecordCommit::new(
                &[logged.write(Some(CommitSequence::from_u64(1)))],
                &control,
            )
            .unwrap(),
            &control,
        )
        .unwrap();
    let assigned = with_value_state(kept, (66, 1, 7));
    let id = store.allocate_transaction(&control).unwrap();
    store
        .commit(
            id,
            &PreparedRecordCommit::new(
                &[
                    assigned.write(Some(CommitSequence::from_u64(2))),
                    RecordWrite {
                        key: dropped.key(),
                        expected: Some(CommitSequence::from_u64(1)),
                        value: None,
                    },
                    RecordWrite {
                        key: values
                            .iter()
                            .find(|value| owner(value) == owner(dropped))
                            .expect("the value record of the dropped definition")
                            .key(),
                        expected: Some(CommitSequence::from_u64(1)),
                        value: None,
                    },
                ],
                &control,
            )
            .unwrap(),
            &control,
        )
        .unwrap();
    drop(store);
    downgrade(&connection, namespace);
    let mut definitions = definitions.into_iter();
    let dropped = definitions.next().expect("the dropped definition");
    let kept = definitions.next().expect("the kept definition");
    (connection, namespace, dropped, kept)
}

#[test]
fn native_sequence_value_upgrade_mirrors_every_retained_definition_revision() {
    let control = StorageReadControl::with_limit(1 << 24);
    let (connection, namespace, dropped, kept) = previous_format_history();
    let (dropped, kept) = (&dropped, &kept);
    let definition_history = versions(&connection, Family::Sequences);
    assert!(versions(&connection, Family::SequenceValues).is_empty());

    // A failed upgrade leaves the previous format and history.
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        initialize_in(&transaction, &control)?;
        check_mapping_version(&transaction, CURRENT_VERSION)?;
        drop(transaction);
        validate_format(sqlite, 13)?;
        Ok(())
    });
    assert!(versions(&connection, Family::SequenceValues).is_empty());

    let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(upgraded.native_namespace(), Some(namespace));
    assert_eq!(versions(&connection, Family::Sequences), definition_history);
    let value_history = versions(&connection, Family::SequenceValues)
        .into_iter()
        .map(|(key, sequence, value)| (key.clone(), sequence, value_state(&key, value.as_deref())))
        .collect::<Vec<_>>();
    let value_key = |definition: &NativeRecord| {
        let (identity, _) = decode_record(definition.key(), definition.row(), &control).unwrap();
        super::super::super::NativeRecordIdentity::new(Family::SequenceValues, identity.owner())
            .unwrap()
            .encode_key(&[], &control)
            .unwrap()
            .to_vec()
    };
    let mut expected = vec![
        (value_key(dropped), 1, Some((1, 0, 0))),
        (value_key(dropped), 3, None),
        (value_key(kept), 1, Some((1, 0, 0))),
        (value_key(kept), 2, Some((33, 1, 0))),
        (value_key(kept), 3, Some((66, 1, 7))),
    ];
    expected.sort();
    assert_eq!(value_history, expected);
    with(&connection, |sqlite| {
        validate_format(sqlite, CURRENT_VERSION)?;
        assert_eq!(
            sqlite
                .prepare("SELECT current, called, log_count FROM _uqa_mvcc_native_sequence_values")?
                .query_map([], |row| Ok(vec![row.get(0)?, row.get(1)?, row.get(2)?]))?
                .collect::<Result<Vec<Vec<Value>>, _>>()?,
            vec![vec![
                Value::Integer(66),
                Value::Integer(1),
                Value::Integer(7)
            ]]
        );
        Ok(())
    });
    let snapshot = upgraded.snapshot(&control).unwrap();
    let current = snapshot.get(&value_key(kept), &control).unwrap().unwrap();
    assert_eq!(
        value_state(&value_key(kept), current.value().map(|value| &***value)),
        Some((66, 1, 7))
    );
    assert!(snapshot
        .get(&value_key(dropped), &control)
        .unwrap()
        .is_none_or(|record| record.value().is_none()));
}
