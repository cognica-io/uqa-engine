//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent `SQLite` vector-search benchmark driven exclusively through SQL.

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

const PROFILE_ENV: &str = "UQA_VECTOR_BENCH_PROFILE";
const OBSERVATIONS_ENV: &str = "UQA_VECTOR_QUALITY_OBSERVATIONS";
const TABLE: &str = "vector_bench";
const INSERT_BATCH_ROWS: usize = 256;

#[derive(Clone, Copy)]
struct Profile {
    name: &'static str,
    corpus_size: usize,
    dimensions: usize,
    quality_query_count: usize,
    performance_query_count: usize,
    top_k: usize,
    query_seed_start: u64,
    ivf_nlist: usize,
    ivf_nprobe: usize,
    ivf_train_threshold: usize,
    hnsw_m: usize,
    hnsw_ef_construction: usize,
    hnsw_ef_search: usize,
    hnsw_rebuild_threshold: usize,
    hnsw_seed: u64,
    sample_size: usize,
    warm_up_seconds: u64,
    measurement_seconds: u64,
}

impl Profile {
    fn selected() -> Self {
        let requested = env::var(PROFILE_ENV).unwrap_or_else(|_| "smoke".into());
        match requested.as_str() {
            "smoke" => Self::smoke(),
            "standard" => Self::standard(),
            "large" => Self::large(),
            other => panic!("unknown {PROFILE_ENV}={other:?}; expected smoke, standard, or large"),
        }
    }

    fn smoke() -> Self {
        Self {
            name: "smoke",
            corpus_size: 10_000,
            dimensions: 32,
            quality_query_count: 100,
            performance_query_count: 25,
            top_k: 10,
            query_seed_start: 1_000_001,
            ivf_nlist: 64,
            ivf_nprobe: 8,
            ivf_train_threshold: 256,
            hnsw_m: 16,
            hnsw_ef_construction: 64,
            hnsw_ef_search: 64,
            hnsw_rebuild_threshold: 1_024,
            hnsw_seed: 7,
            sample_size: 10,
            warm_up_seconds: 1,
            measurement_seconds: 3,
        }
    }

    fn standard() -> Self {
        Self {
            name: "standard",
            corpus_size: 100_000,
            dimensions: 128,
            quality_query_count: 1_000,
            performance_query_count: 25,
            top_k: 10,
            query_seed_start: 10_000_001,
            ivf_nlist: 256,
            ivf_nprobe: 32,
            ivf_train_threshold: 1_024,
            hnsw_m: 16,
            hnsw_ef_construction: 128,
            hnsw_ef_search: 4_096,
            hnsw_rebuild_threshold: 10_000,
            hnsw_seed: 7,
            sample_size: 20,
            warm_up_seconds: 2,
            measurement_seconds: 5,
        }
    }

    fn large() -> Self {
        Self {
            name: "large",
            corpus_size: 1_000_000,
            dimensions: 128,
            quality_query_count: 1_000,
            performance_query_count: 10,
            top_k: 10,
            query_seed_start: 100_000_001,
            ivf_nlist: 1_024,
            ivf_nprobe: 64,
            ivf_train_threshold: 4_096,
            hnsw_m: 16,
            hnsw_ef_construction: 128,
            hnsw_ef_search: 8_192,
            hnsw_rebuild_threshold: 100_000,
            hnsw_seed: 7,
            sample_size: 10,
            warm_up_seconds: 2,
            measurement_seconds: 5,
        }
    }

    fn vector(self, seed: u64) -> Vec<f32> {
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        (0..self.dimensions)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                let unit = ((state >> 32) as u32 as f32) / (u32::MAX as f32);
                unit * 2.0 - 1.0
            })
            .collect()
    }

    fn workload_json(self) -> JsonValue {
        json!({
            "corpus_size": self.corpus_size,
            "dimensions": self.dimensions,
            "quality_query_count": self.quality_query_count,
            "performance_query_count": self.performance_query_count,
            "top_k": self.top_k,
            "generator": "lcg-uniform-signed-f32-v1",
            "corpus_seed_start": 1,
            "query_seed_start": self.query_seed_start,
        })
    }

    fn exact_parameters() -> JsonValue {
        json!({"access_method": "sqlite-bruteforce"})
    }

    fn ivf_parameters(self) -> JsonValue {
        json!({
            "access_method": "sqlite-ivf",
            "nlist": self.ivf_nlist,
            "nprobe": self.ivf_nprobe,
            "train_threshold": self.ivf_train_threshold,
        })
    }

    fn hnsw_parameters(self) -> JsonValue {
        json!({
            "access_method": "sqlite-hnsw",
            "m": self.hnsw_m,
            "ef_construction": self.hnsw_ef_construction,
            "ef_search": self.hnsw_ef_search,
            "rebuild_threshold": self.hnsw_rebuild_threshold,
            "seed": self.hnsw_seed,
        })
    }
}

struct PersistentDatabase {
    _directory: TempDir,
    path: PathBuf,
    load_elapsed_nanoseconds: u64,
}

impl PersistentDatabase {
    fn build(profile: Profile) -> Self {
        let directory = tempdir().expect("create SQL vector benchmark directory");
        let path = directory.path().join("vector-search.sqlite3");
        let engine = Engine::open(&path).expect("open SQL vector benchmark database");
        let started = Instant::now();
        engine
            .sql(
                &format!(
                    "CREATE TABLE {TABLE} (id INTEGER PRIMARY KEY, embedding VECTOR({}))",
                    profile.dimensions
                ),
                &[],
            )
            .expect("create SQL vector benchmark table");
        insert_rows(&engine, profile);
        drop(engine);
        Self {
            _directory: directory,
            path,
            load_elapsed_nanoseconds: elapsed_nanoseconds(started),
        }
    }

    fn reopen(&self) -> Engine {
        Engine::open(&self.path).expect("reopen SQL vector benchmark database")
    }
}

struct ConstructionObservation {
    name: &'static str,
    statement: String,
    elapsed_nanoseconds: u64,
}

struct SqlQueryWorkload {
    profile: Profile,
    sql: String,
    params: Vec<SQLParam>,
    quality_requested: bool,
}

impl SqlQueryWorkload {
    fn new(profile: Profile) -> Self {
        let sql = format!(
            "SELECT id, _score FROM {TABLE} WHERE knn_match(embedding, $1, {}) \
             ORDER BY _score DESC, id ASC LIMIT {}",
            profile.top_k, profile.top_k
        );
        let params = (0..profile.quality_query_count)
            .map(|offset| {
                SQLParam::vector(profile.vector(profile.query_seed_start + offset as u64))
            })
            .collect();
        Self {
            profile,
            sql,
            params,
            quality_requested: observation_path().is_some(),
        }
    }

    fn run_phase(
        &self,
        group: &mut BenchmarkGroup<'_, WallTime>,
        algorithms: &mut Vec<JsonValue>,
        name: &str,
        parameters: &JsonValue,
        engine: &Engine,
    ) {
        if self.quality_requested {
            algorithms.push(algorithm_observations(
                name,
                parameters,
                engine,
                &self.sql,
                &self.params,
                self.profile,
            ));
        }
        benchmark_queries(group, self.profile, name, engine, &self.sql, &self.params);
    }
}

pub(super) fn bench_sql_vector_search(criterion: &mut Criterion) {
    let profile = Profile::selected();
    assert!(profile.performance_query_count <= profile.quality_query_count);
    assert!(profile.top_k <= profile.corpus_size);
    let database = PersistentDatabase::build(profile);
    let workload = SqlQueryWorkload::new(profile);
    let mut algorithms = Vec::with_capacity(3);
    let mut construction = vec![ConstructionObservation {
        name: "sql_load",
        statement: "CREATE TABLE + parameterized batched INSERT".into(),
        elapsed_nanoseconds: database.load_elapsed_nanoseconds,
    }];
    let mut group = criterion.benchmark_group("sql_vector_search_query_batch");
    group.sample_size(profile.sample_size);
    group.warm_up_time(Duration::from_secs(profile.warm_up_seconds));
    group.measurement_time(Duration::from_secs(profile.measurement_seconds));
    group.throughput(Throughput::Elements(profile.performance_query_count as u64));

    let mut engine = database.reopen();
    workload.run_phase(
        &mut group,
        &mut algorithms,
        "exact",
        &Profile::exact_parameters(),
        &engine,
    );

    let ivf_sql = format!(
        "CREATE INDEX vector_bench_embedding_ivf ON {TABLE} USING ivf (embedding) \
         WITH (lists = {}, probes = {}, train_threshold = {})",
        profile.ivf_nlist, profile.ivf_nprobe, profile.ivf_train_threshold
    );
    construction.push(timed_sql(&engine, "sql_create_ivf", &ivf_sql));
    drop(engine);
    engine = database.reopen();
    workload.run_phase(
        &mut group,
        &mut algorithms,
        "ivf",
        &profile.ivf_parameters(),
        &engine,
    );

    engine
        .sql("DROP INDEX vector_bench_embedding_ivf", &[])
        .expect("drop SQL IVF benchmark index");
    let hnsw_sql = format!(
        "CREATE INDEX vector_bench_embedding_hnsw ON {TABLE} USING hnsw (embedding) \
         WITH (m = {}, ef_construction = {}, ef_search = {}, rebuild_threshold = {}, seed = {})",
        profile.hnsw_m,
        profile.hnsw_ef_construction,
        profile.hnsw_ef_search,
        profile.hnsw_rebuild_threshold,
        profile.hnsw_seed
    );
    construction.push(timed_sql(&engine, "sql_create_hnsw", &hnsw_sql));
    drop(engine);
    engine = database.reopen();
    workload.run_phase(
        &mut group,
        &mut algorithms,
        "hnsw",
        &profile.hnsw_parameters(),
        &engine,
    );
    group.finish();
    drop(engine);
    write_observations(profile, &algorithms, &construction);
}

fn insert_rows(engine: &Engine, profile: Profile) {
    let full_insert = insert_statement(INSERT_BATCH_ROWS);
    engine
        .transaction(|engine| {
            for first in (0..profile.corpus_size).step_by(INSERT_BATCH_ROWS) {
                let row_count = INSERT_BATCH_ROWS.min(profile.corpus_size - first);
                let tail_insert;
                let sql = if row_count == INSERT_BATCH_ROWS {
                    &full_insert
                } else {
                    tail_insert = insert_statement(row_count);
                    &tail_insert
                };
                let mut params = Vec::with_capacity(row_count * 2);
                for offset in 0..row_count {
                    let doc_id = first + offset;
                    params.push(SQLParam::scalar(Value::Int(doc_id as i64)));
                    params.push(SQLParam::vector(profile.vector(doc_id as u64 + 1)));
                }
                engine.sql(sql, &params)?;
            }
            Ok(())
        })
        .expect("insert SQL vector benchmark rows");
}

fn insert_statement(row_count: usize) -> String {
    let mut sql = format!("INSERT INTO {TABLE} (id, embedding) VALUES ");
    for row in 0..row_count {
        if row != 0 {
            sql.push_str(", ");
        }
        let first = row * 2 + 1;
        write!(sql, "(${first}, ${})", first + 1).expect("write INSERT placeholders");
    }
    sql
}

fn benchmark_queries(
    group: &mut BenchmarkGroup<'_, WallTime>,
    profile: Profile,
    name: &str,
    engine: &Engine,
    query_sql: &str,
    query_params: &[SQLParam],
) {
    let performance_params = &query_params[..profile.performance_query_count];
    group.bench_function(BenchmarkId::new(profile.name, name), |bencher| {
        bencher.iter(|| {
            let mut returned_rows = 0_usize;
            for query in performance_params {
                let result = engine
                    .sql(black_box(query_sql), std::slice::from_ref(query))
                    .expect("execute SQL vector benchmark query");
                returned_rows += result.rows.len();
                black_box(result.rows);
            }
            black_box(returned_rows)
        });
    });
}

fn algorithm_observations(
    name: &str,
    parameters: &JsonValue,
    engine: &Engine,
    query_sql: &str,
    query_params: &[SQLParam],
    profile: Profile,
) -> JsonValue {
    let results: Vec<JsonValue> = query_params
        .iter()
        .enumerate()
        .map(|(query_id, query)| {
            let result = engine
                .sql(query_sql, std::slice::from_ref(query))
                .expect("execute SQL vector quality query");
            assert_eq!(
                result.rows.len(),
                profile.top_k,
                "{name} returned an incomplete top-k result for query {query_id}"
            );
            let hits: Vec<JsonValue> = result
                .rows
                .iter()
                .map(|row| ranked_hit(row, name, query_id))
                .collect();
            json!({"query_id": query_id, "hits": hits})
        })
        .collect();
    json!({"name": name, "parameters": parameters, "results": results})
}

fn ranked_hit(row: &ResultRow, algorithm: &str, query_id: usize) -> JsonValue {
    let doc_id = match row.get("id") {
        Some(Value::Int(value)) if *value >= 0 => *value as u64,
        value => panic!("{algorithm} query {query_id} returned invalid id {value:?}"),
    };
    let score = match row.get("_score") {
        Some(Value::Float(value)) => *value,
        Some(Value::Int(value)) => *value as f64,
        value => panic!("{algorithm} query {query_id} returned invalid score {value:?}"),
    };
    json!({"doc_id": doc_id, "score": score})
}

fn timed_sql(engine: &Engine, name: &'static str, statement: &str) -> ConstructionObservation {
    let started = Instant::now();
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("execute {name}: {error}"));
    ConstructionObservation {
        name,
        statement: statement.into(),
        elapsed_nanoseconds: elapsed_nanoseconds(started),
    }
}

fn elapsed_nanoseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos())
        .expect("SQL vector benchmark duration fits in u64 nanoseconds")
}

fn observation_path() -> Option<PathBuf> {
    env::var_os(OBSERVATIONS_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn write_observations(
    profile: Profile,
    algorithms: &[JsonValue],
    construction: &[ConstructionObservation],
) {
    let Some(path) = observation_path() else {
        return;
    };
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).expect("create SQL vector observation directory");
    }
    let construction: Vec<JsonValue> = construction
        .iter()
        .map(|stage| {
            json!({
                "name": stage.name,
                "rows": profile.corpus_size,
                "statement": stage.statement,
                "elapsed_nanoseconds": stage.elapsed_nanoseconds,
            })
        })
        .collect();
    let payload = json!({
        "schema_version": 2,
        "profile": profile.name,
        "storage": {
            "backend": "sqlite",
            "persistent": true,
            "reopened_before_each_query_phase": true,
        },
        "execution": {
            "api": "Engine::sql",
            "query": "SELECT ... WHERE knn_match(...) ORDER BY _score DESC, id ASC LIMIT k",
            "index_lifecycle": "SQL CREATE INDEX / DROP INDEX",
        },
        "workload": profile.workload_json(),
        "algorithms": algorithms,
        "construction": construction,
    });
    let encoded = serde_json::to_vec_pretty(&payload).expect("encode SQL vector observations");
    fs::write(Path::new(&path), encoded).expect("write SQL vector observations");
    eprintln!(
        "SQL vector observations ({}): {}",
        profile.name,
        path.display()
    );
}
