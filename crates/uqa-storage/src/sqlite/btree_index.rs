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
use uqa_core::{ArrayValue, DecimalValue, DocId, TemporalValue, Value};

use super::{ManagedConnection, Result, SQLiteError};

fn encode_doc_id(doc_id: DocId) -> Result<i64> {
    i64::try_from(doc_id).map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "document id {doc_id} does not fit in SQLite INTEGER"
        ))
    })
}

fn decode_doc_id(doc_id: i64) -> Result<DocId> {
    DocId::try_from(doc_id).map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "invalid negative document id {doc_id} in persisted B-tree index"
        ))
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
enum StoredValue {
    Null,
    Void,
    Bool(bool),
    Int(i64),
    /// IEEE-754 bits preserve NaN payloads, infinities, and signed zero.
    Float(u64),
    Str(String),
    FixedChar(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(DecimalValue),
    Json(String),
    JsonB(String),
    Array(ArrayValue),
    List(Vec<StoredValue>),
    Row(Vec<StoredValue>),
    Record(Vec<(String, StoredValue)>),
    Map(BTreeMap<String, StoredValue>),
}

impl From<&Value> for StoredValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Void => Self::Void,
            Value::Bool(value) => Self::Bool(*value),
            Value::Int(value) => Self::Int(*value),
            Value::Float(value) => Self::Float(value.to_bits()),
            Value::Str(value) => Self::Str(value.clone()),
            Value::FixedChar(value) => Self::FixedChar(value.clone()),
            Value::Bytes(value) => Self::Bytes(value.clone()),
            Value::Temporal(value) => Self::Temporal(value.clone()),
            Value::Decimal(value) => Self::Decimal(value.clone()),
            Value::Json(value) => Self::Json(value.clone()),
            Value::JsonB(value) => Self::JsonB(value.clone()),
            Value::Array(value) => Self::Array(value.clone()),
            Value::List(values) => Self::List(values.iter().map(Self::from).collect()),
            Value::Row(values) => Self::Row(values.iter().map(Self::from).collect()),
            Value::Record(fields) => Self::Record(
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), Self::from(value)))
                    .collect(),
            ),
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
            Self::Void => Value::Void,
            Self::Bool(value) => Value::Bool(value),
            Self::Int(value) => Value::Int(value),
            Self::Float(bits) => Value::Float(f64::from_bits(bits)),
            Self::Str(value) => Value::Str(value),
            Self::FixedChar(value) => Value::FixedChar(value),
            Self::Bytes(value) => Value::Bytes(value),
            Self::Temporal(value) => Value::Temporal(value),
            Self::Decimal(value) => Value::Decimal(value),
            Self::Json(value) => Value::Json(value),
            Self::JsonB(value) => Value::JsonB(value),
            Self::Array(value) => Value::Array(value),
            Self::List(values) => Value::List(values.into_iter().map(Self::into_value).collect()),
            Self::Row(values) => Value::Row(values.into_iter().map(Self::into_value).collect()),
            Self::Record(fields) => Value::Record(
                fields
                    .into_iter()
                    .map(|(name, value)| (name, value.into_value()))
                    .collect(),
            ),
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

    pub fn fields(&self, table: &str) -> Result<Vec<crate::ValueIndexKey>> {
        self.conn.with(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT field FROM _btree_indexes
                 WHERE table_name = ?1
                 ORDER BY field",
            )?;
            let rows = stmt.query_map([table], |row| row.get::<_, crate::ValueIndexKey>(0))?;
            let mut fields = Vec::new();
            for row in rows {
                fields.push(row?);
            }
            Ok(fields)
        })
    }

    pub fn repairs(&self) -> Result<Vec<(String, crate::ValueIndexKey)>> {
        self.conn.with(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT table_name, field FROM _btree_index_repairs ORDER BY table_name, field",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, crate::ValueIndexKey>(1)?,
                ))
            })?;
            let mut repairs = Vec::new();
            for row in rows {
                repairs.push(row?);
            }
            Ok(repairs)
        })
    }

    pub fn clear_repair(&self, table: &str, field: &crate::ValueIndexKey) -> Result<()> {
        self.conn.with(|conn| {
            conn.execute(
                "DELETE FROM _btree_index_repairs
                 WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            Ok(())
        })
    }

    /// Load a complete persisted index. `None` means this field has not been
    /// built yet and the engine must backfill it from the document store once.
    pub fn load(
        &self,
        table: &str,
        field: &crate::ValueIndexKey,
    ) -> Result<Option<Vec<(DocId, Value)>>> {
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
                let doc_id = decode_doc_id(row.get::<_, i64>(0)?)?;
                let encoded = row.get::<_, String>(1)?;
                values.push((doc_id, decode_value(&encoded)?));
            }
            Ok(Some(values))
        })
    }

    /// Atomically replace the complete persisted posting set and mark it built.
    pub fn replace(
        &self,
        table: &str,
        field: &crate::ValueIndexKey,
        values: &[(DocId, Value)],
    ) -> Result<()> {
        self.replace_many(table, &[(field, values)])
    }

    /// Apply a sparse structural repair while retaining every valid posting.
    /// All ids and values are encoded before opening the savepoint so a range
    /// or serialization error cannot leave half of the delta applied.
    pub fn repair(
        &self,
        table: &str,
        field: &crate::ValueIndexKey,
        stale_doc_ids: &[DocId],
        missing: &[(DocId, Value)],
    ) -> Result<()> {
        let stale_doc_ids = stale_doc_ids
            .iter()
            .map(|doc_id| encode_doc_id(*doc_id))
            .collect::<Result<Vec<_>>>()?;
        let missing = missing
            .iter()
            .map(|(doc_id, value)| Ok((encode_doc_id(*doc_id)?, encode_value(value)?)))
            .collect::<Result<Vec<_>>>()?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "INSERT OR IGNORE INTO _btree_indexes (table_name, field)
                 VALUES (?1, ?2)",
                params![table, field],
            )?;
            {
                let mut delete = tx.prepare_cached(
                    "DELETE FROM _btree_index_entries
                     WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                )?;
                for doc_id in &stale_doc_ids {
                    delete.execute(params![table, field, doc_id])?;
                }
            }
            {
                let mut insert = tx.prepare_cached(
                    "INSERT INTO _btree_index_entries
                       (table_name, field, doc_id, value_json)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (table_name, field, doc_id)
                     DO UPDATE SET value_json = excluded.value_json",
                )?;
                for (doc_id, value_json) in &missing {
                    insert.execute(params![table, field, doc_id, value_json])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// Atomically replace several complete posting sets for one table. Repair
    /// paths commonly rebuild every indexed column together; one savepoint and
    /// one set of prepared statements avoids repeating `SQLite` setup per field.
    pub fn replace_many(
        &self,
        table: &str,
        indexes: &[(&crate::ValueIndexKey, &[(DocId, Value)])],
    ) -> Result<()> {
        let encoded = indexes
            .iter()
            .map(|(field, values)| {
                let values = values
                    .iter()
                    .map(|(doc_id, value)| Ok((encode_doc_id(*doc_id)?, encode_value(value)?)))
                    .collect::<Result<Vec<_>>>()?;
                Ok((*field, values))
            })
            .collect::<Result<Vec<_>>>()?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            {
                let mut delete = tx.prepare_cached(
                    "DELETE FROM _btree_index_entries
                     WHERE table_name = ?1 AND field = ?2",
                )?;
                let mut mark = tx.prepare_cached(
                    "INSERT OR IGNORE INTO _btree_indexes (table_name, field)
                     VALUES (?1, ?2)",
                )?;
                let mut insert = tx.prepare_cached(
                    "INSERT INTO _btree_index_entries
                       (table_name, field, doc_id, value_json)
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                for (field, values) in &encoded {
                    delete.execute(params![table, field])?;
                    mark.execute(params![table, field])?;
                    for (doc_id, value_json) in values {
                        insert.execute(params![table, field, doc_id, value_json])?;
                    }
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
        values: Option<&BTreeMap<crate::ValueIndexKey, Value>>,
    ) -> Result<()> {
        let doc_id = encode_doc_id(doc_id)?;
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
                        stmt.execute(params![table, field, doc_id, value_json])?;
                    }
                }
                None => {
                    tx.execute(
                        "DELETE FROM _btree_index_entries
                         WHERE table_name = ?1 AND doc_id = ?2",
                        params![table, doc_id],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_index(&self, table: &str, field: &crate::ValueIndexKey) -> Result<()> {
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
        conn.with(|connection| {
            connection.execute_batch(
                "INSERT INTO _documents (table_name, doc_id, body)
                 VALUES ('messages', 1, '{}'), ('messages', 2, '{}');",
            )?;
            Ok(())
        })
        .unwrap();
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
            Value::FixedChar("seven   ".into()),
            Value::Bytes(vec![1, 2, 3]),
            Value::Json("{\"b\":2,\"a\":1}".into()),
            Value::JsonB("{\"a\": 1, \"b\": 2}".into()),
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
                &"public_id".into(),
                &[(1, Value::Str("m1".into())), (2, Value::Null)],
            )
            .unwrap();
        assert_eq!(
            store.load("messages", &"public_id".into()).unwrap(),
            Some(vec![(1, Value::Str("m1".into())), (2, Value::Null)])
        );
        assert_eq!(
            store.fields("messages").unwrap(),
            vec![crate::ValueIndexKey::from("public_id")]
        );

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
            store.load("messages", &"public_id".into()).unwrap(),
            Some(vec![(2, Value::Str("m2".into()))])
        );

        store.clear_table("messages").unwrap();
        assert_eq!(
            store.load("messages", &"public_id".into()).unwrap(),
            Some(vec![])
        );
        store.drop_index("messages", &"public_id".into()).unwrap();
        assert_eq!(store.load("messages", &"public_id".into()).unwrap(), None);
    }

    #[test]
    fn sparse_repair_preserves_valid_postings() {
        let store = store();
        store
            .replace(
                "messages",
                &"public_id".into(),
                &[(1, Value::Str("m1".into())), (2, Value::Str("m2".into()))],
            )
            .unwrap();
        let row_id_before: i64 = store
            .conn
            .with(|conn| {
                Ok(conn.query_row(
                    "SELECT rowid FROM _btree_index_entries
                     WHERE table_name = 'messages'
                       AND field = 'public_id' AND doc_id = 2",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        store
            .conn
            .with(|conn| {
                conn.execute(
                    "INSERT INTO _documents (table_name, doc_id, body)
                     VALUES ('messages', 3, '{}')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        store
            .repair(
                "messages",
                &"public_id".into(),
                &[1],
                &[(3, Value::Str("m3".into()))],
            )
            .unwrap();

        assert_eq!(
            store.load("messages", &"public_id".into()).unwrap(),
            Some(vec![
                (2, Value::Str("m2".into())),
                (3, Value::Str("m3".into()))
            ])
        );
        let row_id_after: i64 = store
            .conn
            .with(|conn| {
                Ok(conn.query_row(
                    "SELECT rowid FROM _btree_index_entries
                     WHERE table_name = 'messages'
                       AND field = 'public_id' AND doc_id = 2",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(row_id_after, row_id_before);
    }

    #[test]
    fn out_of_range_document_ids_fail_before_replacing_existing_entries() {
        let store = store();
        store
            .replace(
                "messages",
                &"public_id".into(),
                &[(1, Value::Str("m1".into()))],
            )
            .unwrap();

        let error = store
            .replace(
                "messages",
                &"public_id".into(),
                &[(u64::MAX, Value::Str("overflow".into()))],
            )
            .unwrap_err();
        assert!(error.to_string().contains("does not fit in SQLite INTEGER"));
        assert_eq!(
            store.load("messages", &"public_id".into()).unwrap(),
            Some(vec![(1, Value::Str("m1".into()))])
        );

        let error = store
            .apply_write(
                "messages",
                u64::MAX,
                Some(&BTreeMap::from([(
                    "public_id".into(),
                    Value::Str("overflow".into()),
                )])),
            )
            .unwrap_err();
        assert!(error.to_string().contains("does not fit in SQLite INTEGER"));
    }

    #[test]
    fn negative_persisted_document_id_is_reported_as_corruption() {
        let store = store();
        store
            .replace(
                "messages",
                &"public_id".into(),
                &[(1, Value::Str("m1".into()))],
            )
            .unwrap();
        store
            .conn
            .with(|conn| {
                conn.execute(
                    "INSERT INTO _documents (table_name, doc_id, body)
                     VALUES ('messages', -1, '{}')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO _btree_index_entries
                       (table_name, field, doc_id, value_json)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        "messages",
                        "public_id",
                        -1_i64,
                        encode_value(&Value::Str("corrupt".into()))?
                    ],
                )?;
                Ok(())
            })
            .unwrap();

        let error = store.load("messages", &"public_id".into()).unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid negative document id -1"));
    }
}
