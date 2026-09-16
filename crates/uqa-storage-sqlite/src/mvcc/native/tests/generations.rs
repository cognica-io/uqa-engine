//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::mvcc::{
    CommitFailure, CommitSequence, PreparedRecordCommit, VersionedPersistence,
};

use super::materialization::{delete, initialize, records, replace, with};
use super::*;
use crate::SQLiteRecordStore;

#[test]
fn owner_generation_changes_require_complete_retirement_and_reject_old_writers() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let original = records(&connection, &store, NativeRecordFamily::Documents, &control);
    let owners = records(
        &connection,
        &store,
        NativeRecordFamily::TableOwners,
        &control,
    );
    let generation = [99; 16];
    let changed_owner = replace(&owners[0], 2, ValueRef::Blob(&generation), &control);
    let old = store.snapshot(&control).unwrap();
    let incomplete =
        PreparedRecordCommit::new(&[changed_owner.write(Some(old.sequence()))], &control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    assert!(matches!(
        store.commit(id, &incomplete, &control),
        Err(CommitFailure::Rejected(VersionError::InvalidEncoding(
            "native owner change omits live rows from its old generation"
        )))
    ));
    let mut writes = vec![changed_owner.write(Some(old.sequence()))];
    writes.extend(original.iter().map(delete));
    let complete = PreparedRecordCommit::new(&writes, &control).unwrap();
    let receipt = store.commit(id, &complete, &control).unwrap();
    for record in &original {
        assert_eq!(
            &***old
                .get(record.key(), &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            record.row()
        );
        assert!(store
            .snapshot(&control)
            .unwrap()
            .get(record.key(), &control)
            .unwrap()
            .unwrap()
            .value()
            .is_none());
    }
    let stale =
        PreparedRecordCommit::new(&[original[0].write(Some(receipt.sequence))], &control).unwrap();
    let stale_id = store.allocate_transaction(&control).unwrap();
    assert!(matches!(
        store.commit(stale_id, &stale, &control),
        Err(CommitFailure::Rejected(_))
    ));
    let (identity, values) = decode_record(original[0].key(), original[0].row(), &control).unwrap();
    let NativeRecordOwner::Object {
        identity: object, ..
    } = identity.owner()
    else {
        panic!("table owner")
    };
    let fresh = NativeRecord::encode(
        NativeRecordFamily::Documents,
        NativeRecordOwner::Object {
            identity: object,
            generation,
        },
        &values,
        &control,
    )
    .unwrap();
    assert_ne!(fresh.key(), original[0].key());
    let fresh_batch = PreparedRecordCommit::new(&[fresh.write(None)], &control).unwrap();
    store.commit(stale_id, &fresh_batch, &control).unwrap();
    let latest = store.snapshot(&control).unwrap();
    assert_eq!(
        &***latest
            .get(fresh.key(), &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        fresh.row()
    );
}

#[test]
fn native_catalog_rename_keeps_tuple_identity_and_moves_every_name_binding_atomically() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    with(&connection, |connection| {
        connection.execute_batch("INSERT INTO _relations VALUES ('public', 'docs', 'table'); INSERT INTO _tables(schema_name, relation_name, analyzer, fts_fields, vector_fields) VALUES ('public', 'docs', 'standard', '[]', '[]'); INSERT INTO _relations VALUES ('public', 'numbers', 'sequence'); INSERT INTO _sequences(schema_name, relation_name, start, increment, current) VALUES ('public', 'numbers', 1, 1, 0);")?;
        Ok(())
    });
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let tables = records(&connection, &store, NativeRecordFamily::Tables, &control);
    let relations = records(&connection, &store, NativeRecordFamily::Relations, &control);
    let owners = records(
        &connection,
        &store,
        NativeRecordFamily::TableOwners,
        &control,
    );
    let docs = records(&connection, &store, NativeRecordFamily::Documents, &control);
    let sequences = records(&connection, &store, NativeRecordFamily::Sequences, &control);
    assert_eq!(sequences.len(), 1);
    let (sequence_identity, _) =
        decode_record(sequences[0].key(), sequences[0].row(), &control).unwrap();
    assert!(
        matches!(sequence_identity.owner(), NativeRecordOwner::Object { identity, generation } if identity != [0;16] && generation != [0;16])
    );
    let renamed_table = replace(&tables[0], 1, ValueRef::Text(b"renamed"), &control);
    let renamed_relation = replace(&relations[0], 1, ValueRef::Text(b"renamed"), &control);
    let renamed_owner = replace(&owners[0], 0, ValueRef::Text(b"public.renamed"), &control);
    let renamed_docs: Vec<_> = docs
        .iter()
        .map(|record| replace(record, 0, ValueRef::Text(b"public.renamed"), &control))
        .collect();
    assert_eq!(renamed_table.key(), tables[0].key());
    let mut writes = vec![
        delete(&relations[0]),
        renamed_relation.write(None),
        delete(&owners[0]),
        renamed_owner.write(None),
        renamed_table.write(Some(CommitSequence::from_u64(1))),
    ];
    writes.extend(
        renamed_docs
            .iter()
            .map(|record| record.write(Some(CommitSequence::from_u64(1)))),
    );
    let prepared = PreparedRecordCommit::new(&writes, &control).unwrap();
    let old = store.snapshot(&control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    store.commit(id, &prepared, &control).unwrap();
    for (before, after) in docs.iter().zip(&renamed_docs) {
        assert_eq!(before.key(), after.key());
        assert_eq!(
            &***old
                .get(before.key(), &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            before.row()
        );
        assert_eq!(
            &***store
                .snapshot(&control)
                .unwrap()
                .get(after.key(), &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            after.row()
        );
    }
    with(&connection, |connection| {
        assert_eq!(
            connection.query_row(
                "SELECT count(*) FROM _documents WHERE table_name = 'public.renamed'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            2
        );
        assert_eq!(
            connection.query_row(
                "SELECT count(*) FROM _tables WHERE relation_name = 'renamed'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            1
        );
        assert_eq!(
            connection.query_row(
                "SELECT count(*) FROM _uqa_mvcc_native_owners WHERE name = 'public.docs'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    });
}

#[test]
fn a_new_generation_cannot_overwrite_an_existing_physical_tuple() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let original = records(&connection, &store, NativeRecordFamily::Documents, &control);
    let (_, values) = decode_record(original[0].key(), original[0].row(), &control).unwrap();
    let forged =
        NativeRecord::encode(NativeRecordFamily::Documents, owner(), &values, &control).unwrap();
    let prepared = PreparedRecordCommit::new(&[forged.write(None)], &control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    assert!(matches!(
        store.commit(id, &prepared, &control),
        Err(CommitFailure::Rejected(VersionError::InvalidEncoding(
            "native target is already owned by a different record"
        )))
    ));
    assert_eq!(
        &***store
            .snapshot(&control)
            .unwrap()
            .get(original[0].key(), &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        original[0].row()
    );
}
