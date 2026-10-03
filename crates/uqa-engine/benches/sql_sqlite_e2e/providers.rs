//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public persistent-provider construction and untimed fixture validation.

use std::path::Path;
use std::sync::Arc;

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage_redb::RedbStorage;
use uqa_storage_sqlite::SQLiteKeyValueStorage;

#[derive(Clone, Copy)]
pub(super) enum Provider {
    SQLite,
    SQLiteKeyValue,
    Redb,
}

impl Provider {
    pub(super) fn selected() -> Self {
        match std::env::var("UQA_STORAGE_BENCH_PROVIDER").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("sqlite") => Self::SQLite,
            Ok("sqlite_kv") => Self::SQLiteKeyValue,
            Ok("redb") => Self::Redb,
            other => panic!("invalid UQA_STORAGE_BENCH_PROVIDER: {other:?}"),
        }
    }

    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::SQLite => "sqlite",
            Self::SQLiteKeyValue => "sqlite_kv",
            Self::Redb => "redb",
        }
    }

    pub(super) fn open(self, path: &Path) -> Engine {
        match self {
            Self::SQLite => Engine::open(path).expect("open native SQLite benchmark"),
            Self::SQLiteKeyValue => Engine::from_persistent_provider(Arc::new(
                SQLiteKeyValueStorage::open(path).expect("open SQLite Key/Value benchmark"),
            ))
            .expect("SQLite Key/Value engine"),
            Self::Redb => Engine::from_persistent_provider(Arc::new(
                RedbStorage::open(path).expect("open redb benchmark"),
            ))
            .expect("redb engine"),
        }
    }

    pub(super) fn verify_and_reopen(self, engine: Engine, path: &Path, rows: usize) -> Engine {
        for sql in [
            "BEGIN",
            "UPDATE items SET qty = -1 WHERE id = 1",
            "SAVEPOINT benchmark_fixture",
            "DELETE FROM items WHERE id = 2",
            "ROLLBACK TO SAVEPOINT benchmark_fixture",
        ] {
            engine.sql(sql, &[]).expect("fixture transaction");
        }
        assert_scalar(&engine, "SELECT qty FROM items WHERE id = 1", -1);
        assert_scalar(&engine, "SELECT qty FROM items WHERE id = 2", 2);
        engine.sql("ROLLBACK", &[]).expect("fixture rollback");
        let session = engine.new_session().expect("fixture session");
        assert_scalar(&session, "SELECT qty FROM items WHERE id = 1", 1);
        drop(session);
        drop(engine);

        let reopened = self.open(path);
        assert_scalar(&reopened, "SELECT count(*) FROM items", rows as i64);
        let total: usize = (1..=rows).map(|id| id % 1000).sum();
        assert_scalar(&reopened, "SELECT sum(qty) FROM items", total as i64);
        assert_scalar(&reopened, "SELECT qty FROM items WHERE id = 1", 1);
        reopened
    }
}

fn assert_scalar(engine: &Engine, sql: &str, expected: i64) {
    let result = engine.sql(sql, &[]).expect("fixture query");
    assert_eq!(result.rows.len(), 1, "{sql}");
    assert_eq!(
        result.rows[0].values().next(),
        Some(&Value::Int(expected)),
        "{sql}"
    );
}
