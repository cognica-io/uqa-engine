//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The public statement adapter reaches native collection through its retained table owner.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{DocId, Value};
use uqa_storage::{DocumentStore, SharedDocumentRow, StorageBackendResult, StoredDocument};

#[derive(Default)]
struct Counts {
    shared: AtomicUsize,
    borrowed: AtomicUsize,
}

struct Observed {
    inner: Arc<dyn DocumentStore>,
    counts: Arc<Counts>,
}

impl DocumentStore for Observed {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("read-only query")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("read-only query")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("read-only query")
    }
    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.inner.get_stored(id)
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.inner.doc_ids()
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.inner.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(Self {
            inner: self.inner.snapshot()?,
            counts: Arc::clone(&self.counts),
        }))
    }
    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, SharedDocumentRow)>>> {
        let rows = self.inner.next_shared_fields(after, limit, fields)?;
        self.counts
            .shared
            .fetch_add(rows.as_ref().map_or(0, Vec::len), Ordering::Relaxed);
        Ok(rows)
    }
    fn for_each_next_shared_fields_borrowed(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> Option<usize> {
        let visited = self
            .inner
            .for_each_next_shared_fields_borrowed(after, limit, fields, visitor)?;
        self.counts.borrowed.fetch_add(visited, Ordering::Relaxed);
        Some(visited)
    }
}

#[test]
fn public_sql_collects_stored_values_without_per_row_shared_ownership() {
    let engine = crate::Engine::new();
    engine
        .sql("CREATE TABLE native_results (id BIGINT PRIMARY KEY, body TEXT, quantity BIGINT, extended_price BIGINT)", &[])
        .unwrap();
    engine.sql("INSERT INTO native_results SELECT n, CASE WHEN n % 3 = 0 THEN NULL ELSE '日本λ-' || n::text END, n % 50, n * 10 FROM generate_series(1, 128) AS g(n)", &[]).unwrap();
    let counts = Arc::new(Counts::default());
    {
        let table = engine.require_table("native_results").unwrap();
        let mut documents = table.document_store.write();
        let inner = std::mem::replace(
            &mut *documents,
            Box::new(uqa_storage::MemoryDocumentStore::new()),
        );
        *documents = Box::new(Observed {
            inner: inner.into(),
            counts: Arc::clone(&counts),
        });
    }
    let result = engine
        .sql("SELECT body AS same, id AS same FROM native_results", &[])
        .unwrap();
    assert_eq!(result.columns, ["same", "same"]);
    assert_eq!(
        result.column_types,
        [
            Some(uqa_sql::ColumnType::Text),
            Some(uqa_sql::ColumnType::BigInteger)
        ]
    );
    assert_eq!(result.rows.len(), 128);
    for position in 0..128 {
        let id = i64::try_from(position + 1).unwrap();
        let body = if id % 3 == 0 {
            Value::Null
        } else {
            Value::Str(format!("日本λ-{id}"))
        };
        assert_eq!(result.value_at(position, 0), Some(&body));
        assert_eq!(result.value_at(position, 1), Some(&Value::Int(id)));
        assert_eq!(result.rows[position]["same"], body);
        assert_eq!(result.rows[position]["same_1"], Value::Int(id));
    }
    assert_eq!(counts.borrowed.load(Ordering::Relaxed), 128);
    assert_eq!(counts.shared.load(Ordering::Relaxed), 0);

    counts.borrowed.store(0, Ordering::Relaxed);
    let result = engine
        .sql(
            "SELECT id, extended_price FROM native_results WHERE quantity < 40 ORDER BY id",
            &[],
        )
        .unwrap();
    let expected = (1_i64..=128).filter(|id| id % 50 < 40).collect::<Vec<_>>();
    assert_eq!(result.rows.len(), expected.len());
    for (position, id) in expected.into_iter().enumerate() {
        assert_eq!(result.value_at(position, 0), Some(&Value::Int(id)));
        assert_eq!(result.value_at(position, 1), Some(&Value::Int(id * 10)));
    }
    assert_eq!(counts.borrowed.load(Ordering::Relaxed), 128);
    assert_eq!(counts.shared.load(Ordering::Relaxed), 0);
}
