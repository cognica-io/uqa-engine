//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic `SQLite` persistence for the shared block-max score index.

use rusqlite::params;
use std::collections::BTreeMap;
use uqa_storage::{BlockMaxIndex, StorageBackendError, TokenTermKey};

pub trait SQLiteBlockMaxPersistence {
    fn save_to_sqlite(&self, connection: &rusqlite::Connection) -> rusqlite::Result<()>;
    fn load_from_sqlite(&mut self, connection: &rusqlite::Connection) -> rusqlite::Result<()>;
}

impl SQLiteBlockMaxPersistence for BlockMaxIndex {
    fn save_to_sqlite(&self, conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        ensure_global_blockmax_shape(conn)?;
        let transaction = conn.unchecked_transaction()?;
        transaction.execute("DELETE FROM _global_blockmax", [])?;
        for ((table, field, term), scores) in self.entries() {
            for (block_idx, score) in scores.iter().enumerate() {
                let block_idx = i64::try_from(block_idx)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
                transaction.execute(
                    "INSERT INTO _global_blockmax
                        (table_name, field, term, block_idx, max_score)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![table, field, term.as_bytes(), block_idx, *score],
                )?;
            }
        }
        transaction.commit()
    }

    fn load_from_sqlite(&mut self, conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        ensure_global_blockmax_shape(conn)?;
        let mut stmt = conn.prepare(
            "SELECT table_name, field, term, block_idx, max_score
             FROM _global_blockmax
             ORDER BY table_name, field, term, block_idx",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                read_term_key(row.get_ref(2)?)?,
                row.get::<_, i64>(3)?,
                row.get::<_, f64>(4)?,
            ))
        })?;
        let mut loaded = BTreeMap::<(String, String, TokenTermKey), Vec<f64>>::new();
        for row in rows {
            let (table, field, term, block_idx, score) = row?;
            let idx = usize::try_from(block_idx)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, block_idx))?;
            let entry = loaded.entry((table, field, term)).or_default();
            if idx != entry.len() {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Integer,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "invalid block-max ordinal sequence: expected {}, found {idx}",
                            entry.len()
                        ),
                    )),
                ));
            }
            entry.push(score);
        }
        let mut replacement = Self::new(self.block_size()).map_err(storage_error_to_sqlite)?;
        for ((table, field, term), scores) in loaded {
            replacement
                .set_block_maxes_key(&table, &field, &term, scores)
                .map_err(storage_error_to_sqlite)?;
        }
        *self = replacement;
        Ok(())
    }
}

fn read_term_key(value: rusqlite::types::ValueRef<'_>) -> rusqlite::Result<TokenTermKey> {
    match value {
        rusqlite::types::ValueRef::Blob(bytes) => {
            TokenTermKey::from_bytes(bytes.to_vec()).map_err(storage_error_to_sqlite)
        }
        rusqlite::types::ValueRef::Text(bytes) => std::str::from_utf8(bytes)
            .map(TokenTermKey::from_text)
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            }),
        _ => Err(rusqlite::Error::InvalidColumnType(
            2,
            "term".into(),
            value.data_type(),
        )),
    }
}

fn storage_error_to_sqlite(error: StorageBackendError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

fn ensure_global_blockmax_shape(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _global_blockmax (
            table_name TEXT NOT NULL DEFAULT '',
            field     TEXT NOT NULL,
            term      BLOB NOT NULL,
            block_idx INTEGER NOT NULL,
            max_score REAL NOT NULL,
            PRIMARY KEY (table_name, field, term, block_idx)
        )",
        [],
    )?;
    let mut stmt = conn.prepare("PRAGMA table_info(_global_blockmax)")?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    if !cols.iter().any(|c| c == "table_name") {
        conn.execute(
            "ALTER TABLE _global_blockmax ADD COLUMN table_name TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_persisted_ordinal_does_not_replace_loaded_state() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        ensure_global_blockmax_shape(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO _global_blockmax
                    (table_name, field, term, block_idx, max_score)
                 VALUES ('docs', 'body', 'bad', -1, 9.0)",
                [],
            )
            .unwrap();
        let mut index = BlockMaxIndex::default();
        index
            .set_block_maxes("old", "body", "term", vec![1.0])
            .unwrap();

        assert!(index.load_from_sqlite(&connection).is_err());
        assert_eq!(index.block_max("old", "body", "term", 0), 1.0);
    }

    #[test]
    fn failed_save_rolls_back_deleted_snapshot() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        ensure_global_blockmax_shape(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO _global_blockmax
                    (table_name, field, term, block_idx, max_score)
                 VALUES ('old', 'body', 'term', 0, 1.0)",
                [],
            )
            .unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_blockmax_insert
                 BEFORE INSERT ON _global_blockmax
                 BEGIN
                     SELECT RAISE(ABORT, 'injected block-max failure');
                 END;",
            )
            .unwrap();
        let mut index = BlockMaxIndex::default();
        index
            .set_block_maxes("new", "body", "term", vec![2.0])
            .unwrap();

        assert!(index.save_to_sqlite(&connection).is_err());
        let persisted: (String, f64) = connection
            .query_row(
                "SELECT table_name, max_score FROM _global_blockmax",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(persisted, ("old".to_string(), 1.0));
    }
}
