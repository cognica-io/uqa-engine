//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::{Engine, ScoringMode};
use uqa_storage::{
    HNSWIndexParams, IVFIndexParams, PersistentStorageProvider, VectorIndexOpenMode,
    VectorIndexSpec,
};
use uqa_storage_redb::RedbStorage;

fn open_engine(path: &std::path::Path) -> Engine {
    let storage: Arc<dyn PersistentStorageProvider> =
        Arc::new(RedbStorage::open(path).expect("open redb storage"));
    Engine::from_persistent_provider(storage).expect("open engine over redb")
}

#[test]
fn engine_runs_text_vector_and_relational_queries_after_reopen() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("engine.redb");

    {
        let engine = open_engine(&path);
        engine
            .sql(
                "CREATE TABLE articles (
                    id INTEGER PRIMARY KEY,
                    title TEXT,
                    embedding VECTOR(2)
                )",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX articles_title_gin ON articles USING gin (title)",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO articles (id, title, embedding) VALUES
                 (1, 'rust redb search', ARRAY[1.0, 0.0]),
                 (2, 'portable key value', ARRAY[0.0, 1.0])",
                &[],
            )
            .unwrap();

        let hits = engine
            .search("articles", "title", "redb", &ScoringMode::default(), 10)
            .unwrap();
        assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
        let vectors = engine
            .knn_search("articles", "embedding", vec![0.0, 1.0], 1)
            .unwrap();
        assert_eq!(vectors.first().map(|hit| hit.doc_id), Some(2));
    }

    let reopened = open_engine(&path);
    let result = reopened
        .sql("SELECT title FROM articles WHERE id = 2", &[])
        .unwrap();
    assert_eq!(
        result.rows[0].get("title"),
        Some(&Value::Str("portable key value".into()))
    );
    assert_eq!(
        reopened
            .search("articles", "title", "rust", &ScoringMode::default(), 10)
            .unwrap()
            .first()
            .map(|hit| hit.doc_id),
        Some(1)
    );
}

#[test]
fn engine_sessions_share_commits_but_not_transaction_state() {
    let directory = tempdir().unwrap();
    let root = open_engine(&directory.path().join("sessions.redb"));
    root.sql(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, note TEXT)",
        &[],
    )
    .unwrap();
    let writer = root.new_session().unwrap();
    let observer = root.new_session().unwrap();

    writer.begin().unwrap();
    writer
        .sql("INSERT INTO items (id, note) VALUES (1, 'kept')", &[])
        .unwrap();
    writer.savepoint("after_first").unwrap();
    writer
        .sql(
            "INSERT INTO items (id, note) VALUES (2, 'rolled back')",
            &[],
        )
        .unwrap();
    writer.rollback_to_savepoint("after_first").unwrap();
    writer.release_savepoint("after_first").unwrap();
    writer.commit().unwrap();

    let result = observer
        .sql("SELECT id, note FROM items ORDER BY id", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(1)));
    assert_eq!(result.rows[0].get("note"), Some(&Value::Str("kept".into())));
}

#[test]
fn engine_rollback_restores_catalog_and_documents_together() {
    let directory = tempdir().unwrap();
    let engine = open_engine(&directory.path().join("rollback.redb"));

    engine.begin().unwrap();
    engine
        .sql("CREATE TABLE discarded (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO discarded (id) VALUES (1)", &[])
        .unwrap();
    engine.rollback().unwrap();

    assert!(!engine.has_table("discarded").unwrap());
    assert!(engine
        .sql("SELECT id FROM discarded", &[])
        .unwrap_err()
        .to_string()
        .contains("does not exist"));
}

#[test]
fn redb_persists_btree_postings_across_reopen_and_mutation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("btree.redb");
    {
        let engine = open_engine(&path);
        engine
            .sql(
                "CREATE TABLE items (id INTEGER PRIMARY KEY, price INTEGER, name TEXT)",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX items_price_idx ON items USING btree (price)",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO items (id, price, name) VALUES
                 (1, 10, 'one'), (2, 20, 'two'), (3, 30, 'three')",
                &[],
            )
            .unwrap();
        engine
            .sql("UPDATE items SET price = 25 WHERE id = 2", &[])
            .unwrap();
        engine.sql("DELETE FROM items WHERE id = 1", &[]).unwrap();
    }

    let reopened = open_engine(&path);
    let result = reopened
        .sql(
            "SELECT id FROM items WHERE price BETWEEN 20 AND 30 ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(2)));
    assert_eq!(result.rows[1].get("id"), Some(&Value::Int(3)));
    drop(reopened);

    let storage = RedbStorage::open(&path).unwrap();
    let session = storage.open_session().unwrap();
    assert!(session.backend.persists_btree_indexes());
    assert_eq!(
        session.backend.btree_index_fields("public.items").unwrap(),
        vec!["id", "price"]
    );
    assert_eq!(
        session
            .backend
            .load_btree_index("public.items", "price")
            .unwrap(),
        Some(vec![(2, Value::Int(25)), (3, Value::Int(30))])
    );
}

#[test]
fn redb_ivf_and_hnsw_survive_reopen_and_incremental_changes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("vector-indexes.redb");
    create_vector_index_database(&path);
    mutate_vector_indexes_after_reopen(&path);
    let reopened = open_engine(&path);
    assert_eq!(
        nearest_doc(&reopened, "ivf_docs", "embedding", [1.0, 0.0]),
        2
    );
    assert_eq!(
        nearest_doc(&reopened, "hnsw_docs", "embedding", [0.0, 1.0]),
        1
    );
}

fn create_vector_index_database(path: &std::path::Path) {
    let engine = open_engine(path);
    engine
        .sql(
            "CREATE TABLE ivf_docs (id INTEGER PRIMARY KEY, embedding VECTOR(2));
             CREATE TABLE hnsw_docs (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    for table in ["ivf_docs", "hnsw_docs"] {
        engine
            .sql(
                &format!(
                    "INSERT INTO {table} (id, embedding) VALUES
                     (1, ARRAY[1.0, 0.0]), (2, ARRAY[0.0, 1.0]), (3, ARRAY[-1.0, 0.0])"
                ),
                &[],
            )
            .unwrap();
    }
    engine
        .sql(
            "CREATE INDEX ivf_docs_embedding_idx ON ivf_docs USING ivf (embedding)
             WITH (lists = 2, probes = 2, train_threshold = 2);
             CREATE INDEX hnsw_docs_embedding_idx ON hnsw_docs USING hnsw (embedding)
             WITH (m = 4, ef_construction = 16, ef_search = 16,
                   rebuild_threshold = 2, seed = 7)",
            &[],
        )
        .unwrap();
    assert_eq!(nearest_doc(&engine, "ivf_docs", "embedding", [1.0, 0.0]), 1);
    assert_eq!(
        nearest_doc(&engine, "hnsw_docs", "embedding", [-1.0, 0.0]),
        3
    );
}

fn mutate_vector_indexes_after_reopen(path: &std::path::Path) {
    let engine = open_engine(path);
    for (name, kind) in [
        ("ivf_docs_embedding_idx", "ivf"),
        ("hnsw_docs_embedding_idx", "hnsw"),
    ] {
        assert_eq!(
            engine.catalog_index(name).unwrap().unwrap().index_type,
            kind
        );
    }
    engine
        .sql(
            "UPDATE ivf_docs SET embedding = ARRAY[0.9, 0.1] WHERE id = 2;
             DELETE FROM ivf_docs WHERE id = 1;
             UPDATE hnsw_docs SET embedding = ARRAY[0.1, 0.9] WHERE id = 1;
             DELETE FROM hnsw_docs WHERE id = 2",
            &[],
        )
        .unwrap();
    assert_eq!(nearest_doc(&engine, "ivf_docs", "embedding", [1.0, 0.0]), 2);
    assert_eq!(
        nearest_doc(&engine, "hnsw_docs", "embedding", [0.0, 1.0]),
        1
    );
}

fn nearest_doc(engine: &Engine, table: &str, field: &str, query: [f32; 2]) -> u64 {
    engine.knn_search(table, field, query, 1).unwrap()[0].doc_id
}

#[test]
fn redb_rolls_back_btree_ivf_and_hnsw_state_atomically() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index-rollback.redb");
    let engine = open_engine(&path);
    engine
        .sql(
            "CREATE TABLE docs (
                id INTEGER PRIMARY KEY,
                rank INTEGER,
                ivf_vector VECTOR(2),
                hnsw_vector VECTOR(2)
             );
             CREATE INDEX docs_rank_idx ON docs USING btree (rank);
             INSERT INTO docs (id, rank, ivf_vector, hnsw_vector) VALUES
                (1, 10, ARRAY[1.0, 0.0], ARRAY[1.0, 0.0]),
                (2, 20, ARRAY[0.0, 1.0], ARRAY[0.0, 1.0]);
             CREATE INDEX docs_ivf_idx ON docs USING ivf (ivf_vector)
                WITH (lists = 2, probes = 2, train_threshold = 2);
             CREATE INDEX docs_hnsw_idx ON docs USING hnsw (hnsw_vector)
                WITH (m = 4, ef_construction = 16, ef_search = 16, seed = 7)",
            &[],
        )
        .unwrap();

    engine.begin().unwrap();
    engine
        .sql(
            "UPDATE docs SET rank = 99,
                ivf_vector = ARRAY[-1.0, 0.0],
                hnsw_vector = ARRAY[-1.0, 0.0]
             WHERE id = 1;
             DELETE FROM docs WHERE id = 2",
            &[],
        )
        .unwrap();
    engine.rollback().unwrap();

    let result = engine
        .sql(
            "SELECT id FROM docs WHERE rank BETWEEN 10 AND 20 ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        engine
            .knn_search("docs", "ivf_vector", vec![1.0, 0.0], 1)
            .unwrap()[0]
            .doc_id,
        1
    );
    assert_eq!(
        engine
            .knn_search("docs", "hnsw_vector", vec![0.0, 1.0], 1)
            .unwrap()[0]
            .doc_id,
        2
    );

    drop(engine);
    let reopened = open_engine(&path);
    assert_eq!(
        reopened
            .knn_search("docs", "ivf_vector", vec![1.0, 0.0], 1)
            .unwrap()[0]
            .doc_id,
        1
    );
    assert_eq!(
        reopened
            .knn_search("docs", "hnsw_vector", vec![0.0, 1.0], 1)
            .unwrap()[0]
            .doc_id,
        2
    );
}

#[test]
fn redb_index_caches_follow_savepoints_and_sibling_commits() {
    let directory = tempdir().unwrap();
    let root = open_engine(&directory.path().join("index-sessions.redb"));
    root.sql(
        "CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            rank INTEGER,
            ivf_vector VECTOR(2),
            hnsw_vector VECTOR(2)
         );
         CREATE INDEX docs_rank_idx ON docs USING btree (rank);
         INSERT INTO docs (id, rank, ivf_vector, hnsw_vector) VALUES
            (1, 10, ARRAY[1.0, 0.0], ARRAY[1.0, 0.0]),
            (2, 20, ARRAY[0.0, 1.0], ARRAY[0.0, 1.0]);
         CREATE INDEX docs_ivf_idx ON docs USING ivf (ivf_vector)
            WITH (lists = 2, probes = 2, train_threshold = 2);
         CREATE INDEX docs_hnsw_idx ON docs USING hnsw (hnsw_vector)
            WITH (m = 4, ef_construction = 16, ef_search = 16, seed = 7)",
        &[],
    )
    .unwrap();
    let writer = root.new_session().unwrap();
    let observer = root.new_session().unwrap();

    writer.begin().unwrap();
    writer.savepoint("before_indexes").unwrap();
    writer
        .sql(
            "UPDATE docs SET rank = 99,
                ivf_vector = ARRAY[-1.0, 0.0],
                hnsw_vector = ARRAY[-1.0, 0.0]
             WHERE id = 1",
            &[],
        )
        .unwrap();
    writer.rollback_to_savepoint("before_indexes").unwrap();
    writer.release_savepoint("before_indexes").unwrap();
    writer.commit().unwrap();
    assert_eq!(
        observer
            .knn_search("docs", "ivf_vector", vec![1.0, 0.0], 1)
            .unwrap()[0]
            .doc_id,
        1
    );
    assert_eq!(
        observer
            .sql("SELECT rank FROM docs WHERE id = 1", &[])
            .unwrap()
            .rows[0]
            .get("rank"),
        Some(&Value::Int(10))
    );

    writer
        .sql(
            "UPDATE docs SET rank = 99,
                ivf_vector = ARRAY[-1.0, 0.0],
                hnsw_vector = ARRAY[-1.0, 0.0]
             WHERE id = 1",
            &[],
        )
        .unwrap();
    assert_eq!(
        observer
            .sql("SELECT rank FROM docs WHERE id = 1", &[])
            .unwrap()
            .rows[0]
            .get("rank"),
        Some(&Value::Int(99))
    );
    assert_eq!(
        observer
            .knn_search("docs", "ivf_vector", vec![1.0, 0.0], 1)
            .unwrap()[0]
            .doc_id,
        2
    );
    assert_eq!(
        observer
            .knn_search("docs", "hnsw_vector", vec![1.0, 0.0], 1)
            .unwrap()[0]
            .doc_id,
        2
    );
}

#[test]
fn redb_physical_indexes_follow_table_and_column_lifecycle() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index-lifecycle.redb");
    create_rename_and_mutate_indexed_table(&path);
    truncate_and_reinsert_after_reopen(&path);
    drop_indexed_columns_after_reopen(&path);
    assert_indexed_columns_are_physically_removed(&path);
    open_engine(&path).sql("DROP TABLE archived", &[]).unwrap();
    assert_table_indexes_are_physically_removed(&path);
}

fn create_rename_and_mutate_indexed_table(path: &std::path::Path) {
    let engine = open_engine(path);
    engine
        .sql(
            "CREATE TABLE docs (
                id INTEGER PRIMARY KEY,
                rank INTEGER,
                ivf_vector VECTOR(2),
                hnsw_vector VECTOR(2)
             );
             CREATE INDEX docs_rank_idx ON docs USING btree (rank);
             INSERT INTO docs (id, rank, ivf_vector, hnsw_vector) VALUES
                (1, 10, ARRAY[1.0, 0.0], ARRAY[1.0, 0.0]),
                (2, 20, ARRAY[0.0, 1.0], ARRAY[0.0, 1.0]);
             CREATE INDEX docs_ivf_idx ON docs USING ivf (ivf_vector)
                WITH (lists = 2, probes = 2, train_threshold = 2);
             CREATE INDEX docs_hnsw_idx ON docs USING hnsw (hnsw_vector)
                WITH (m = 4, ef_construction = 16, ef_search = 16, seed = 7);
             ALTER TABLE docs RENAME TO archived;
             ALTER TABLE archived RENAME COLUMN rank TO score;
             ALTER TABLE archived RENAME COLUMN ivf_vector TO ivf_embedding;
             ALTER TABLE archived RENAME COLUMN hnsw_vector TO hnsw_embedding;
             UPDATE archived SET score = 30,
                ivf_embedding = ARRAY[-1.0, 0.0],
                hnsw_embedding = ARRAY[-1.0, 0.0]
              WHERE id = 1;
             INSERT INTO archived (id, score, ivf_embedding, hnsw_embedding)
                VALUES (3, 15, ARRAY[1.0, 0.0], ARRAY[1.0, 0.0])",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT id FROM archived WHERE score BETWEEN 10 AND 20 ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        nearest_doc(&engine, "archived", "ivf_embedding", [1.0, 0.0]),
        3
    );
    assert_eq!(
        nearest_doc(&engine, "archived", "hnsw_embedding", [0.0, 1.0]),
        2
    );
}

fn truncate_and_reinsert_after_reopen(path: &std::path::Path) {
    let engine = open_engine(path);
    assert!(!engine.has_table("docs").unwrap());
    assert!(engine.has_table("archived").unwrap());
    assert_eq!(
        nearest_doc(&engine, "archived", "ivf_embedding", [1.0, 0.0]),
        3
    );
    assert_eq!(
        nearest_doc(&engine, "archived", "hnsw_embedding", [0.0, 1.0]),
        2
    );
    engine
        .sql(
            "TRUNCATE archived;
             INSERT INTO archived (id, score, ivf_embedding, hnsw_embedding)
                VALUES (4, 40, ARRAY[0.5, 0.5], ARRAY[0.5, 0.5])",
            &[],
        )
        .unwrap();
    assert_eq!(
        nearest_doc(&engine, "archived", "ivf_embedding", [0.5, 0.5]),
        4
    );
}

fn drop_indexed_columns_after_reopen(path: &std::path::Path) {
    let engine = open_engine(path);
    let rows = engine.sql("SELECT id, score FROM archived", &[]).unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get("id"), Some(&Value::Int(4)));
    assert_eq!(
        nearest_doc(&engine, "archived", "hnsw_embedding", [0.5, 0.5]),
        4
    );
    engine
        .sql(
            "ALTER TABLE archived DROP COLUMN score;
             ALTER TABLE archived DROP COLUMN ivf_embedding;
             ALTER TABLE archived DROP COLUMN hnsw_embedding",
            &[],
        )
        .unwrap();
}

fn assert_indexed_columns_are_physically_removed(path: &std::path::Path) {
    let storage = RedbStorage::open(path).unwrap();
    let session = storage.open_session().unwrap();
    assert_eq!(
        session
            .backend
            .btree_index_fields("public.archived")
            .unwrap(),
        vec!["id"]
    );
    assert_vector_index_is_missing(
        session.backend.as_ref(),
        "ivf_embedding",
        lifecycle_ivf_spec(),
    );
    assert_vector_index_is_missing(
        session.backend.as_ref(),
        "hnsw_embedding",
        lifecycle_hnsw_spec(),
    );
}

fn assert_vector_index_is_missing(
    backend: &dyn uqa_storage::PersistentStorageBackend,
    field: &str,
    spec: VectorIndexSpec,
) {
    assert!(backend
        .vector_index(
            "public.archived",
            field,
            2,
            spec,
            VectorIndexOpenMode::Restore,
        )
        .is_err());
}

fn lifecycle_ivf_spec() -> VectorIndexSpec {
    VectorIndexSpec::IVF(IVFIndexParams {
        nlist: 2,
        nprobe: 2,
        train_threshold: 2,
    })
}

fn lifecycle_hnsw_spec() -> VectorIndexSpec {
    VectorIndexSpec::HNSW(HNSWIndexParams {
        m: 4,
        ef_construction: 16,
        ef_search: 16,
        rebuild_threshold: HNSWIndexParams::default().rebuild_threshold,
        seed: 7,
    })
}

fn assert_table_indexes_are_physically_removed(path: &std::path::Path) {
    let storage = RedbStorage::open(path).unwrap();
    let session = storage.open_session().unwrap();
    assert!(session
        .backend
        .btree_index_fields("public.archived")
        .unwrap()
        .is_empty());
}
