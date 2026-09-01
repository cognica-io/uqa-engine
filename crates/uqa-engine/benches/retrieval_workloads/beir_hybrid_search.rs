//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent SQL-only hybrid retrieval over real BEIR embeddings.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use criterion::measurement::WallTime;
use criterion::{black_box, BenchmarkGroup, BenchmarkId, Criterion, Throughput};
use serde_json::{json, Value as JsonValue};
use tempfile::{tempdir, TempDir};
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::{ResultRow, SQLParam};

#[path = "beir_hybrid_search/dataset.rs"]
mod dataset;

use dataset::{BenchmarkManifest, DocumentCase, Fixture, QueryCase, SystemSpec};

pub(super) const DATA_DIR_ENV: &str = "UQA_BEIR_DATA_DIR";
const MANIFEST_ENV: &str = "UQA_BEIR_BENCH_MANIFEST";
const OBSERVATIONS_ENV: &str = "UQA_BEIR_OBSERVATIONS";
const TABLE: &str = "beir_documents";

struct PersistentDatabase {
    _directory: TempDir,
    path: PathBuf,
    construction: Vec<JsonValue>,
}

impl PersistentDatabase {
    fn build(manifest: &BenchmarkManifest, documents: Vec<DocumentCase>) -> Self {
        let directory = tempdir().expect("create BEIR database directory");
        let path = directory.path().join("beir.sqlite3");
        let engine = Engine::open(&path).expect("open persistent BEIR database");
        let started = Instant::now();
        engine
            .sql(
                &format!(
                    "CREATE TABLE {TABLE} (id INTEGER PRIMARY KEY, source_id TEXT, body TEXT, embedding VECTOR({}))",
                    manifest.embedding.dimensions
                ),
                &[],
            )
            .expect("create BEIR SQL table");
        insert_documents(&engine, documents, manifest.workload.insert_batch_rows);
        let mut construction = vec![construction_observation(
            "sql_load",
            &manifest.execution.load,
            manifest.dataset.expected_corpus_count,
            started,
        )];
        for index in &manifest.indexes {
            let started = Instant::now();
            engine
                .sql(&index.statement, &[])
                .unwrap_or_else(|error| panic!("execute {}: {error}", index.name));
            construction.push(construction_observation(
                &index.name,
                &index.statement,
                manifest.dataset.expected_corpus_count,
                started,
            ));
        }
        drop(engine);
        Self {
            _directory: directory,
            path,
            construction,
        }
    }

    fn reopen(&self) -> Engine {
        Engine::open(&self.path).expect("reopen persistent BEIR database")
    }
}

pub(super) fn bench_beir_hybrid_search(criterion: &mut Criterion) {
    let manifest_path = manifest_path();
    let data_directory = required_path(DATA_DIR_ENV);
    let observation_path = required_path(OBSERVATIONS_ENV);
    let fixture = Fixture::load(&manifest_path, &data_directory);
    let Fixture {
        manifest,
        manifest_sha256,
        preparation,
        artifacts,
        documents,
        queries,
    } = fixture;
    let database = PersistentDatabase::build(&manifest, documents);
    let engine = database.reopen();
    verify_persisted_row_count(&engine, manifest.dataset.expected_corpus_count);
    let systems = quality_observations(&engine, &manifest, &queries);
    benchmark_queries(criterion, &engine, &manifest, &queries);
    drop(engine);
    write_observations(
        &observation_path,
        &manifest_path,
        &manifest,
        &manifest_sha256,
        &preparation,
        &artifacts,
        &database.construction,
        &queries,
        &systems,
    );
}

fn insert_documents(engine: &Engine, documents: Vec<DocumentCase>, batch_rows: usize) {
    let mut documents = documents.into_iter();
    engine
        .transaction(|engine| {
            loop {
                let batch: Vec<_> = documents.by_ref().take(batch_rows).collect();
                if batch.is_empty() {
                    break;
                }
                let sql = insert_statement(batch.len());
                let mut params = Vec::with_capacity(batch.len() * 4);
                for document in batch {
                    params.push(SQLParam::scalar(Value::Int(document.id as i64)));
                    params.push(SQLParam::scalar(Value::Str(document.source_id)));
                    params.push(SQLParam::scalar(Value::Str(document.body)));
                    params.push(SQLParam::vector(document.embedding));
                }
                engine.sql(&sql, &params)?;
            }
            Ok(())
        })
        .expect("insert BEIR documents through SQL");
}

fn insert_statement(row_count: usize) -> String {
    let mut sql = format!("INSERT INTO {TABLE} (id, source_id, body, embedding) VALUES ");
    for row in 0..row_count {
        if row > 0 {
            sql.push_str(", ");
        }
        let first = row * 4 + 1;
        write!(
            sql,
            "(${first}, ${}, ${}, ${})",
            first + 1,
            first + 2,
            first + 3
        )
        .expect("write BEIR INSERT placeholders");
    }
    sql
}

fn quality_observations(
    engine: &Engine,
    manifest: &BenchmarkManifest,
    queries: &[QueryCase],
) -> Vec<JsonValue> {
    manifest
        .systems
        .iter()
        .map(|system| {
            let sql = manifest.query_sql(system);
            let results = queries
                .iter()
                .map(|query| {
                    let result = engine.sql(sql, &query.params).unwrap_or_else(|error| {
                        panic!("execute {} quality SQL: {error}", system.name)
                    });
                    assert!(result.rows.len() <= manifest.workload.top_k);
                    if system.require_top_k {
                        assert_eq!(result.rows.len(), manifest.workload.top_k);
                    }
                    let hits = result
                        .rows
                        .iter()
                        .map(|row| ranked_hit(row, &system.name, &query.id))
                        .collect::<Vec<_>>();
                    json!({"query_id": query.id, "hits": hits})
                })
                .collect::<Vec<_>>();
            json!({"name": system.name, "results": results})
        })
        .collect()
}

fn benchmark_queries(
    criterion: &mut Criterion,
    engine: &Engine,
    manifest: &BenchmarkManifest,
    queries: &[QueryCase],
) {
    let measurement = &manifest.measurement;
    let performance_queries = &queries[..manifest.workload.performance_query_count];
    let mut group = criterion.benchmark_group("beir_hybrid_query_batch");
    group.sample_size(measurement.sample_size);
    group.warm_up_time(Duration::from_secs(measurement.warm_up_seconds));
    group.measurement_time(Duration::from_secs(measurement.measurement_seconds));
    group.throughput(Throughput::Elements(performance_queries.len() as u64));
    for system in &manifest.systems {
        benchmark_system(
            &mut group,
            engine,
            &manifest.dataset.name,
            system,
            manifest.query_sql(system),
            performance_queries,
        );
    }
    group.finish();
}

fn benchmark_system(
    group: &mut BenchmarkGroup<'_, WallTime>,
    engine: &Engine,
    dataset: &str,
    system: &SystemSpec,
    sql: &str,
    queries: &[QueryCase],
) {
    group.bench_function(BenchmarkId::new(dataset, &system.name), |bencher| {
        bencher.iter(|| {
            let mut rows = 0_usize;
            for query in queries {
                let result = engine
                    .sql(black_box(sql), black_box(&query.params))
                    .unwrap_or_else(|error| {
                        panic!("execute {} benchmark SQL: {error}", system.name)
                    });
                rows += result.rows.len();
                black_box(result.rows);
            }
            black_box(rows)
        });
    });
}

fn ranked_hit(row: &ResultRow, system: &str, query_id: &str) -> JsonValue {
    let document_id = match row.get("id") {
        Some(Value::Int(value)) if *value > 0 => *value as u64,
        other => panic!("{system} query {query_id} returned invalid id {other:?}"),
    };
    let score = match row.get("_score") {
        Some(Value::Float(value)) => *value,
        Some(Value::Int(value)) => *value as f64,
        other => panic!("{system} query {query_id} returned invalid score {other:?}"),
    };
    assert!(score.is_finite(), "{system} query {query_id} score");
    json!({"doc_id": document_id, "score": score})
}

fn verify_persisted_row_count(engine: &Engine, expected: usize) {
    let result = engine
        .sql(&format!("SELECT COUNT(*) AS n FROM {TABLE}"), &[])
        .expect("count reopened BEIR rows");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("n"), Some(&Value::Int(expected as i64)));
}

fn construction_observation(
    name: &str,
    statement: &str,
    rows: usize,
    started: Instant,
) -> JsonValue {
    json!({
        "name": name,
        "statement": statement,
        "rows": rows,
        "elapsed_nanoseconds": u64::try_from(started.elapsed().as_nanos())
            .expect("BEIR construction duration fits in u64"),
    })
}

#[expect(clippy::too_many_arguments, reason = "names every workload control")]
fn write_observations(
    path: &Path,
    manifest_path: &Path,
    manifest: &BenchmarkManifest,
    manifest_sha256: &str,
    preparation: &JsonValue,
    artifacts: &std::collections::BTreeMap<String, dataset::ArtifactSpec>,
    construction: &[JsonValue],
    queries: &[QueryCase],
    systems: &[JsonValue],
) {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).expect("create BEIR observation directory");
    }
    let judgments = queries
        .iter()
        .map(|query| json!({"query_id": query.id, "judgments": query.judgments}))
        .collect::<Vec<_>>();
    let payload = json!({
        "schema_version": 1,
        "benchmark_manifest": manifest_path.display().to_string(),
        "benchmark_manifest_sha256": manifest_sha256,
        "dataset": manifest.dataset,
        "embedding": manifest.embedding,
        "storage": manifest.storage,
        "execution": manifest.execution,
        "indexes": manifest.indexes,
        "workload": manifest.workload,
        "preparation": preparation,
        "artifacts": artifacts,
        "construction": construction,
        "queries": judgments,
        "systems": systems,
    });
    fs::write(
        path,
        serde_json::to_vec_pretty(&payload).expect("encode BEIR observations"),
    )
    .expect("write BEIR observations");
    eprintln!("BEIR SQL observations: {}", path.display());
}

fn manifest_path() -> PathBuf {
    env::var_os(MANIFEST_ENV).map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/beir/manifest.json"),
        PathBuf::from,
    )
}

fn required_path(name: &str) -> PathBuf {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || panic!("{name} is required for the BEIR benchmark"),
            PathBuf::from,
        )
}
