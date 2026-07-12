//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent backing for logical `btree` value indexes.
//!
//! The engine still uses its in-memory [`crate::BTreeIndex`] for query-time
//! scans, but the compact `(table, field, doc_id, value)` rows live in `SQLite`.
//! Reopening an engine hydrates the B-tree from these rows instead of parsing
//! every full document again. Writes replace the affected postings in the
//! active `SQLite` transaction as the document mutation.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uqa_core::{DecimalValue, DocId, TemporalValue, Value};

use super::{ManagedConnection, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
enum StoredValue {
    Null,
    Bool(bool),
    Int(i64),
    /// IEEE-754 bits preserve NaN payloads, infinities, and signed zero.
    Float(u64),
    Str(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(DecimalValue),
    List(Vec<StoredValue>),
    Map(BTreeMap<String, StoredValue>),
}

impl From<&Value> for StoredValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(*value),
            Value::Int(value) => Self::Int(*value),
            Value::Float(value) => Self::Float(value.to_bits()),
            Value::Str(value) => Self::Str(value.clone()),
            Value::Bytes(value) => Self::Bytes(value.clone()),
            Value::Temporal(value) => Self::Temporal(value.clone()),
            Value::Decimal(value) => Self::Decimal(value.clone()),
            Value::List(values) => Self::List(values.iter().map(Self::from).collect()),
            Value::Map(values) => Self::Map(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), Self::from(value)))
                    .collect(),
            ),
        }
    }
}

impl StoredValue {
    fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(value),
            Self::Int(value) => Value::Int(value),
            Self::Float(bits) => Value::Float(f64::from_bits(bits)),
            Self::Str(value) => Value::Str(value),
            Self::Bytes(value) => Value::Bytes(value),
            Self::Temporal(value) => Value::Temporal(value),
            Self::Decimal(value) => Value::Decimal(value),
            Self::List(values) => Value::List(values.into_iter().map(Self::into_value).collect()),
            Self::Map(values) => Value::Map(
                values
                    .into_iter()
                    .map(|(key, value)| (key, value.into_value()))
                    .collect(),
            ),
        }
    }
}

fn encode_value(value: &Value) -> Result<String> {
    Ok(serde_json::to_string(&StoredValue::from(value))?)
}

fn decode_value(encoded: &str) -> Result<Value> {
    Ok(serde_json::from_str::<StoredValue>(encoded)?.into_value())
}

#[derive(Clone)]
pub struct SQLiteBTreeIndexStore {
    conn: ManagedConnection,
}

impl SQLiteBTreeIndexStore {
    pub fn new(conn: ManagedConnection) -> Self {
        Self { conn }
    }

    pub fn fields(&self, table: &str) -> Result<Vec<String>> {
        self.conn.with(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT field FROM _btree_indexes
                 WHERE table_name = ?1
                 ORDER BY field",
            )?;
            let rows = stmt.query_map([table], |row| row.get::<_, String>(0))?;
            let mut fields = Vec::new();
            for row in rows {
                fields.push(row?);
            }
            Ok(fields)
        })
    }

    /// Load a complete persisted index. `None` means this field has not been
    /// built yet and the engine must backfill it from the document store once.
    pub fn load(&self, table: &str, field: &str) -> Result<Option<Vec<(DocId, Value)>>> {
        self.conn.with(|conn| {
            let exists = conn
                .prepare_cached(
                    "SELECT 1 FROM _btree_indexes
                     WHERE table_name = ?1 AND field = ?2",
                )?
                .query_row(params![table, field], |row| row.get::<_, i64>(0))
                .optional()?
                .is_some();
            if !exists {
                return Ok(None);
            }

            let mut stmt = conn.prepare_cached(
                "SELECT doc_id, value_json
                 FROM _btree_index_entries
                 WHERE table_name = ?1 AND field = ?2
                 ORDER BY doc_id",
            )?;
            let mut rows = stmt.query(params![table, field])?;
            let mut values = Vec::new();
            while let Some(row) = rows.next()? {
                let doc_id = row.get::<_, i64>(0)? as DocId;
                let encoded = row.get::<_, String>(1)?;
                values.push((doc_id, decode_value(&encoded)?));
            }
            Ok(Some(values))
        })
    }

    /// Atomically replace the complete persisted posting set and mark it built.
    pub fn replace(&self, table: &str, field: &str, values: &[(DocId, Value)]) -> Result<()> {
        let encoded = values
            .iter()
            .map(|(doc_id, value)| Ok((*doc_id, encode_value(value)?)))
            .collect::<Result<Vec<_>>>()?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _btree_index_entries
                 WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO _btree_indexes (table_name, field)
                 VALUES (?1, ?2)",
                params![table, field],
            )?;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO _btree_index_entries
                       (table_name, field, doc_id, value_json)
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                for (doc_id, value_json) in &encoded {
                    stmt.execute(params![table, field, *doc_id as i64, value_json])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// Apply one document write to every persisted field currently loaded by
    /// the engine. A replacement uses the `(table, field, doc_id)` primary key,
    /// so updates never need a separate old-value delete.
    pub fn apply_write(
        &self,
        table: &str,
        doc_id: DocId,
        values: Option<&BTreeMap<String, Value>>,
    ) -> Result<()> {
        let encoded = values
            .map(|values| {
                values
                    .iter()
                    .map(|(field, value)| Ok((field.clone(), encode_value(value)?)))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            match encoded.as_ref() {
                Some(values) => {
                    let mut stmt = tx.prepare_cached(
                        "INSERT INTO _btree_index_entries
                           (table_name, field, doc_id, value_json)
                         SELECT ?1, ?2, ?3, ?4
                         WHERE EXISTS (
                             SELECT 1 FROM _btree_indexes
                             WHERE table_name = ?1 AND field = ?2
                         )
                         ON CONFLICT (table_name, field, doc_id)
                         DO UPDATE SET value_json = excluded.value_json",
                    )?;
                    for (field, value_json) in values {
                        stmt.execute(params![table, field, doc_id as i64, value_json])?;
                    }
                }
                None => {
                    tx.execute(
                        "DELETE FROM _btree_index_entries
                         WHERE table_name = ?1 AND doc_id = ?2",
                        params![table, doc_id as i64],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_index(&self, table: &str, field: &str) -> Result<()> {
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _btree_index_entries
                 WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            tx.execute(
                "DELETE FROM _btree_indexes
                 WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// TRUNCATE keeps the index definitions but removes every posting.
    pub fn clear_table(&self, table: &str) -> Result<()> {
        self.conn.with(|conn| {
            conn.execute(
                "DELETE FROM _btree_index_entries WHERE table_name = ?1",
                params![table],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::Catalog;

    fn store() -> SQLiteBTreeIndexStore {
        let conn = ManagedConnection::open_in_memory().unwrap();
        let _catalog = Catalog::open(conn.clone()).unwrap();
        SQLiteBTreeIndexStore::new(conn)
    }

    #[test]
    fn tagged_values_round_trip_without_untagged_serde_ambiguity() {
        let values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(7),
            Value::Float(-0.0),
            Value::Float(f64::NAN),
            Value::Str("seven".into()),
            Value::Bytes(vec![1, 2, 3]),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::Map(BTreeMap::from([("k".into(), Value::Str("v".into()))])),
        ];
        for value in values {
            let decoded = decode_value(&encode_value(&value).unwrap()).unwrap();
            match (&value, &decoded) {
                (Value::Float(left), Value::Float(right)) if left.is_nan() => {
                    assert!(right.is_nan());
                }
                _ => assert_eq!(decoded, value),
            }
        }
    }

    #[test]
    fn replace_load_write_delete_and_clear_round_trip() {
        let store = store();
        store
            .replace(
                "messages",
                "public_id",
                &[(1, Value::Str("m1".into())), (2, Value::Null)],
            )
            .unwrap();
        assert_eq!(
            store.load("messages", "public_id").unwrap(),
            Some(vec![(1, Value::Str("m1".into())), (2, Value::Null)])
        );
        assert_eq!(store.fields("messages").unwrap(), vec!["public_id"]);

        store
            .apply_write(
                "messages",
                2,
                Some(&BTreeMap::from([
                    ("public_id".into(), Value::Str("m2".into())),
                    ("not_built".into(), Value::Int(9)),
                ])),
            )
            .unwrap();
        store.apply_write("messages", 1, None).unwrap();
        assert_eq!(
            store.load("messages", "public_id").unwrap(),
            Some(vec![(2, Value::Str("m2".into()))])
        );

        store.clear_table("messages").unwrap();
        assert_eq!(store.load("messages", "public_id").unwrap(), Some(vec![]));
        store.drop_index("messages", "public_id").unwrap();
        assert_eq!(store.load("messages", "public_id").unwrap(), None);
    }
}
