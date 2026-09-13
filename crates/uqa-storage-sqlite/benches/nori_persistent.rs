//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` Nori batch commits and rollbacks with reopened occurrence verification.

use std::path::Path;
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteInvertedIndex};

#[path = "../../../benchmarks/nori/persistent.rs"]
mod persistent;

struct Session {
    index: SQLiteInvertedIndex,
    connection: ManagedConnection,
}

impl persistent::Session for Session {
    type Index = SQLiteInvertedIndex;

    fn open(path: &Path) -> Self {
        let connection = ManagedConnection::open(path).unwrap();
        Catalog::open(connection.clone()).unwrap();
        let index = SQLiteInvertedIndex::new(
            connection.clone(),
            "docs",
            uqa_analysis::whitespace_analyzer(),
        );
        Self { index, connection }
    }

    fn index(&mut self) -> &mut Self::Index {
        &mut self.index
    }

    fn begin(&self) {
        self.connection.begin_transaction().unwrap();
    }

    fn finish(&self, rollback: bool) {
        if rollback {
            self.connection.rollback_transaction().unwrap();
        } else {
            self.connection.commit_transaction().unwrap();
        }
    }
}

fn main() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("settings.db")).unwrap();
    let settings = connection
        .with(|conn| {
            let mode: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            let sync: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
            let page_size: i64 = conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
            let version: String =
                conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
            Ok(format!(
                "SQLite {version}, journal_mode={mode}, synchronous={sync}, page_size={page_size}"
            ))
        })
        .unwrap();
    drop(connection);
    persistent::run::<Session>("uqa-storage-sqlite", &settings);
}
