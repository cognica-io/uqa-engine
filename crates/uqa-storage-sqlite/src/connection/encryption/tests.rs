//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::path::Path;

use rusqlite::Connection;
use uqa_storage::{
    encryption_key::AuxiliaryFile, read_control::StorageReadControl, StorageEncryptionKey,
};

use super::PASSWORD_DERIVATIONS;
use crate::{notifications::NotificationRegistry, ManagedConnection, SQLiteRecordStore};

const PASSWORD: &str = "local café d'essai — 암호";
const RAW: &str = "X'00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff'";

fn connections_and_state(path: &Path, input: &str) {
    let main = ManagedConnection::open_encrypted(path, input).unwrap();
    let keys = main.auxiliary_encryption_key().unwrap();
    let path = path.canonicalize().unwrap();
    let leases: Vec<_> = (0..3).map(|_| main.lease_connection().unwrap()).collect();
    for lease in &leases {
        assert!(
            lease
                .query_row("SELECT count(*) FROM sqlite_schema", [], |r| r
                    .get::<_, i64>(0))
                .unwrap()
                >= 0
        );
    }
    drop(leases);
    let roles = [
        AuxiliaryFile::Notifications,
        AuxiliaryFile::Serializable,
        AuxiliaryFile::RelationIdentities,
    ];
    let keys_by_role = roles.map(|file| keys.for_file(file, &file.path(&path)).unwrap());
    for key in &keys_by_role {
        assert_eq!(super::raw_hex(key.expose_secret()).map(str::len), Some(64));
    }
    for i in 0..roles.len() {
        for j in 0..i {
            assert!(keys_by_role[i].expose_secret() != keys_by_role[j].expose_secret());
        }
    }
    let registry = NotificationRegistry::open(&path, Some(&keys)).unwrap();
    let tx = registry.begin().unwrap();
    assert!(tx.allocate_backend_process_id().unwrap() > 0);
    tx.commit().unwrap();
    let records = SQLiteRecordStore::new(&main).unwrap();
    records
        .serializable_admission(&StorageReadControl::with_limit(1 << 20))
        .unwrap();
    let relation = ManagedConnection::open_auxiliary(
        &AuxiliaryFile::RelationIdentities.path(&path),
        Some(keys_by_role[2].clone()),
    )
    .unwrap();
    relation
        .with(|db| {
            Ok(db.execute_batch("CREATE TABLE IF NOT EXISTS identities(id INTEGER PRIMARY KEY)")?)
        })
        .unwrap();
}

#[rstest::rstest]
#[case::passphrase(PASSWORD, 1)]
#[case::raw_key(RAW, 0)]
fn encrypted_open_bounds_password_derivations_across_connections_and_state(
    #[case] input: &str,
    #[case] expected: usize,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keyed.db");
    for _ in 0..2 {
        PASSWORD_DERIVATIONS.set(0);
        connections_and_state(&path, input);
        assert_eq!(PASSWORD_DERIVATIONS.get(), expected);
    }
    // Independent SQLCipher clients still open the main database with the original input.
    let external = Connection::open(&path).unwrap();
    external.pragma_update(None, "key", input).unwrap();
    let count: i64 = external
        .query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))
        .unwrap();
    assert!(count > 0);
}

#[test]
fn encrypted_auxiliary_keys_bind_the_role_to_the_main_file_salt() {
    let dir = tempfile::tempdir().unwrap();
    let key_for = |name| {
        let path = dir.path().join(name);
        let main = ManagedConnection::open_encrypted(&path, RAW).unwrap();
        let keys = main.auxiliary_encryption_key().unwrap();
        let side = AuxiliaryFile::Notifications.path(&path);
        assert!(keys.for_file(AuxiliaryFile::Serializable, &side).is_err());
        keys.for_file(AuxiliaryFile::Notifications, &side).unwrap()
    };
    assert!(key_for("a").expose_secret() != key_for("b").expose_secret());
    assert!(key_for("a").expose_secret() == key_for("a").expose_secret());
}

fn next_process(registry: &NotificationRegistry) -> i32 {
    let tx = registry.begin().unwrap();
    let id = tx.allocate_backend_process_id().unwrap();
    tx.commit().unwrap();
    id
}

#[test]
fn encrypted_legacy_notification_conversion_keeps_state_and_active_owners() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notifications.db");
    let old_parent = ManagedConnection::open_encrypted(&path, PASSWORD).unwrap();
    let retained_credential = old_parent.auxiliary_encryption_key().unwrap();
    let original = StorageEncryptionKey::new(PASSWORD);
    let old = NotificationRegistry::open(&path, Some(&original)).unwrap();
    assert_eq!(next_process(&old), 1);
    let current = ManagedConnection::open_encrypted(&path, PASSWORD).unwrap();
    let shared =
        NotificationRegistry::open(&path, current.auxiliary_encryption_key().as_ref()).unwrap();
    assert_eq!(next_process(&shared), 2);
    assert_eq!(next_process(&old), 3);
    drop((old, old_parent, shared, current));
    for next in [4, 5] {
        PASSWORD_DERIVATIONS.set(0);
        let main = ManagedConnection::open_encrypted(&path, PASSWORD).unwrap();
        let migrated =
            NotificationRegistry::open(&path, main.auxiliary_encryption_key().as_ref()).unwrap();
        assert_eq!(next_process(&migrated), next);
        assert_eq!(PASSWORD_DERIVATIONS.get(), if next == 4 { 2 } else { 1 });
    }
    assert!(NotificationRegistry::open(&path, Some(&original)).is_err());
    assert_eq!(
        next_process(&NotificationRegistry::open(&path, Some(&retained_credential)).unwrap()),
        6
    );
}

#[rstest::rstest]
#[case::sqlcipher(false)]
#[case::compressed(true)]
fn simultaneous_encrypted_first_opens_use_the_persisted_salt(#[case] compressed: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.db");
    let open = || {
        if compressed {
            ManagedConnection::open_compressed_encrypted(
                &path,
                PASSWORD,
                crate::SQLiteCompressionOptions::default(),
            )
        } else {
            ManagedConnection::open_encrypted(&path, PASSWORD)
        }
        .unwrap()
    };
    let start = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4).map(|_| scope.spawn(|| {
            start.wait();
            let main = open();
            main.with(|connection| Ok(connection.execute_batch("CREATE TABLE IF NOT EXISTS shared(value); INSERT INTO shared VALUES (1)")?)).unwrap();
        })).collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
    let main = open();
    assert_eq!(
        main.with(
            |db| Ok(db.query_row("SELECT count(*) FROM shared", [], |r| r.get::<_, i64>(0))?)
        )
        .unwrap(),
        4
    );
}

#[test]
fn compressed_first_openers_share_the_published_auxiliary_keys() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("empty-compressed.db");
    let open = || {
        ManagedConnection::open_compressed_encrypted(
            &path,
            PASSWORD,
            crate::SQLiteCompressionOptions::default(),
        )
        .unwrap()
    };
    // Both pools open before either caller writes a SQLite page.
    let first = open();
    let second = open();
    let role = AuxiliaryFile::Notifications;
    let selected = |main: &ManagedConnection| {
        main.auxiliary_encryption_key()
            .unwrap()
            .for_file(role, &role.path(&path))
            .unwrap()
    };
    let first_key = selected(&first);
    assert!(
        first_key.expose_secret() == selected(&second).expose_secret(),
        "independent empty openers selected different notification keys"
    );
    second
        .with(|sqlite| Ok(sqlite.execute_batch("CREATE TABLE durable(value)")?))
        .unwrap();
    for main in [&first, &second] {
        let registry =
            NotificationRegistry::open(&path, main.auxiliary_encryption_key().as_ref()).unwrap();
        assert!(next_process(&registry) > 0);
    }
    drop((first, second));
    let reopened = open();
    assert!(first_key.expose_secret() == selected(&reopened).expose_secret());
    assert_eq!(
        next_process(
            &NotificationRegistry::open(&path, reopened.auxiliary_encryption_key().as_ref())
                .unwrap()
        ),
        3
    );
}
