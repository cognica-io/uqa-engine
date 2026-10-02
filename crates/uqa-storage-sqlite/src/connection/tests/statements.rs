//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared statements a connection retains across transactions.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use uqa_core::Value;
use uqa_storage::{mvcc::VersionedSessionOptions, DocumentStore};

use crate::{Catalog, ManagedConnection, SQLiteDocumentStore};

/// Records what `SQLite` authorizes while it prepares statements, other than transaction control and pragmas, which it prepares again for every run. Installing an authorizer expires every prepared statement, so it stays installed and records only while asked to.
struct Preparations {
    recording: Arc<AtomicBool>,
    actions: Arc<Mutex<Vec<String>>>,
}

impl Preparations {
    fn watch(connection: &ManagedConnection) -> Self {
        let recording = Arc::new(AtomicBool::new(false));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let (enabled, seen) = (Arc::clone(&recording), Arc::clone(&actions));
        connection
            .with_physical(|sqlite| {
                sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                    if enabled.load(Ordering::Relaxed)
                        && !matches!(
                            context.action,
                            AuthAction::Transaction { .. }
                                | AuthAction::Savepoint { .. }
                                | AuthAction::Pragma { .. }
                        )
                    {
                        seen.lock().unwrap().push(format!("{:?}", context.action));
                    }
                    Authorization::Allow
                }))?;
                Ok(())
            })
            .unwrap();
        Self { recording, actions }
    }

    fn during(&self, operation: impl FnOnce()) -> Vec<String> {
        self.actions.lock().unwrap().clear();
        self.recording.store(true, Ordering::Relaxed);
        operation();
        self.recording.store(false, Ordering::Relaxed);
        let actions = self.actions.lock().unwrap().clone();
        actions
    }
}

#[test]
fn a_repeated_native_write_prepares_no_statement() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let preparations = Preparations::watch(&connection);
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    let document = || BTreeMap::from([("value".into(), Value::Int(1))]);
    documents.put(1, document()).unwrap();
    documents.put(2, document()).unwrap();
    assert_eq!(
        preparations.during(|| documents.put(3, document()).unwrap()),
        Vec::<String>::new()
    );
    documents.delete(1).unwrap();
    assert_eq!(
        preparations.during(|| documents.delete(2).unwrap()),
        Vec::<String>::new()
    );
    assert_eq!(documents.len().unwrap(), 1);
}
