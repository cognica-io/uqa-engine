//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent vector writers follow the completed catalog after a racing maintenance commit.

use std::path::Path;
use std::sync::{atomic::Ordering, Arc};

use uqa_storage::{HNSWIndexParams, IVFIndexParams, PersistentStorageProvider, VectorIndexSpec};
use uqa_storage_sqlite::{ManagedConnection, SQLiteCompressionOptions, SQLiteStorageProvider};

use super::{Engine, Value};

fn pause_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
}

fn open(provider: usize, path: &Path) -> Engine {
    let provider: Arc<dyn PersistentStorageProvider> = match provider {
        0..=3 => {
            let connection = match provider {
                0 => ManagedConnection::open(path),
                1 => ManagedConnection::open_encrypted(path, "vector publication fixture"),
                2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
                3 => ManagedConnection::open_compressed_encrypted(
                    path,
                    "vector publication fixture",
                    SQLiteCompressionOptions::default(),
                ),
                _ => unreachable!(),
            }
            .unwrap();
            Arc::new(SQLiteStorageProvider::new(connection))
        }
        4 => Arc::new(uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap()),
        5 => Arc::new(uqa_storage_redb::RedbStorage::open(path).unwrap()),
        _ => unreachable!(),
    };
    let engine = Engine::from_persistent_provider(provider).unwrap();
    pause_statistics(&engine);
    engine
}

fn spec(method: &str) -> (VectorIndexSpec, Vec<(String, String)>) {
    match method {
        "ivf" => (
            VectorIndexSpec::IVF(IVFIndexParams {
                nlist: 1,
                nprobe: 1,
                train_threshold: 1,
            }),
            vec![
                ("lists".into(), "1".into()),
                ("probes".into(), "1".into()),
                ("train_threshold".into(), "1".into()),
            ],
        ),
        "hnsw" => (
            VectorIndexSpec::HNSW(HNSWIndexParams::default()),
            Vec::new(),
        ),
        _ => unreachable!(),
    }
}

fn ids(engine: &Engine) -> Vec<Value> {
    engine
        .sql(
            "SELECT id FROM docs WHERE knn_match(embedding, ARRAY[1.0,0.0], 10) ORDER BY id",
            &[],
        )
        .unwrap()
        .rows
        .iter()
        .map(|row| row["id"].clone())
        .collect()
}

#[rstest::rstest]
#[case::ivf("ivf")]
#[case::hnsw("hnsw")]
fn concurrent_vector_registration_retains_completed_bindings(#[case] method: &str) {
    let directory = tempfile::tempdir().unwrap();
    std::thread::scope(|scope| {
        for provider in 0..6 {
            let path = directory.path().join(format!("{method}-{provider}.db"));
            scope.spawn(move || {
                let engine = open(provider, &path);
                engine.sql("CREATE TABLE docs(id int PRIMARY KEY, embedding vector(2)); INSERT INTO docs VALUES(9, ARRAY[-1.0,0.0]); CREATE TABLE unrelated(id int)", &[]).unwrap();
                let peer = engine.new_session().unwrap();
                pause_statistics(&peer);
                for commit in [false, true] {
                    engine.sql("BEGIN", &[]).unwrap();
                    engine.with_implicit_storage_definition_transaction(|engine| {
                        let (spec, options) = spec(method);
                        engine.rebuild_vector_field_in_transaction("docs", "embedding", 2, spec)?;
                        // Force CREATE INDEX's build/reservation interleaving without relying on the background worker's timer or host load.
                        peer.sql("ANALYZE unrelated", &[]).unwrap();
                        engine.register_catalog_index("docs_embedding", method, "docs", &["embedding".into()], &options)
                    }).unwrap();
                    let table = engine.try_table("docs").unwrap().unwrap();
                    assert!(table.vector_indexes.read()["embedding"].index_kind().ends_with(method), "provider {provider} lost the {method} writer");
                    engine.sql("INSERT INTO docs VALUES(1, ARRAY[1.0,0.0])", &[]).unwrap();
                    assert_eq!(ids(&engine), [Value::Int(1), Value::Int(9)]);
                    assert_eq!(ids(&peer), [Value::Int(9)]);
                    engine.sql(if commit { "COMMIT" } else { "ROLLBACK" }, &[]).unwrap();
                    assert_eq!(engine.has_catalog_index("docs_embedding").unwrap(), commit);
                    if !commit {
                        assert_eq!(ids(&engine), [Value::Int(9)]);
                    }
                }
                peer.sql("INSERT INTO docs VALUES(2, ARRAY[0.0,1.0])", &[]).unwrap();
                let expected = [Value::Int(1), Value::Int(2), Value::Int(9)];
                assert_eq!(ids(&engine), expected);
                assert_eq!(ids(&peer), expected);
                drop(peer);
                drop(engine);
                assert_eq!(ids(&open(provider, &path)), expected);
            });
        }
    });
}

#[rstest::rstest]
#[case::ivf("ivf")]
#[case::hnsw("hnsw")]
fn catalog_publication_retains_completed_persistent_handles(#[case] method: &str) {
    let directory = tempfile::tempdir().unwrap();
    for provider in 0..6 {
        let engine = open(
            provider,
            &directory.path().join(format!("{method}-{provider}.db")),
        );
        engine.sql("CREATE TABLE docs(id int PRIMARY KEY, embedding vector(2)); INSERT INTO docs VALUES(9, ARRAY[-1.0,0.0])", &[]).unwrap();
        engine
            .with_implicit_storage_definition_transaction(|engine| {
                let (spec, options) = spec(method);
                engine.rebuild_vector_field_in_transaction("docs", "embedding", 2, spec)?;
                let table = engine.try_table("docs")?.unwrap();
                let built =
                    std::ptr::from_ref(&table.vector_indexes.read()["embedding"]).cast::<()>();
                engine.register_catalog_index(
                    "docs_embedding",
                    method,
                    "docs",
                    &["embedding".into()],
                    &options,
                )?;
                let published = engine.try_table("docs")?.unwrap();
                assert_eq!(
                    built,
                    std::ptr::from_ref(&published.vector_indexes.read()["embedding"]).cast::<()>(),
                    "provider {provider} needlessly reopened the completed {method} build"
                );
                Ok(())
            })
            .unwrap();
        engine
            .sql("INSERT INTO docs VALUES(1, ARRAY[1.0,0.0])", &[])
            .unwrap();
        assert_eq!(ids(&engine), [Value::Int(1), Value::Int(9)]);
    }
}

#[rstest::rstest]
#[case::memory(false)]
#[case::temporary(true)]
fn catalog_publication_retains_memory_vector_contents(#[case] temporary: bool) {
    let directory = tempfile::tempdir().unwrap();
    for method in ["ivf", "hnsw"] {
        let engine = if temporary {
            open(0, &directory.path().join(format!("{method}.db")))
        } else {
            Engine::new()
        };
        let persistence = if temporary { "TEMP" } else { "" };
        engine.sql(&format!("CREATE {persistence} TABLE docs(id int PRIMARY KEY, embedding vector(2)); INSERT INTO docs VALUES(9, ARRAY[-1.0,0.0]); CREATE INDEX docs_embedding ON docs USING {method}(embedding); INSERT INTO docs VALUES(1, ARRAY[1.0,0.0])"), &[]).unwrap();
        assert_eq!(ids(&engine), [Value::Int(1), Value::Int(9)]);
    }
}
