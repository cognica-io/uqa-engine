//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unified index-build and warm-search benchmark for text, vector, graph, and hybrid retrieval. Every timed search returns the shared posting-list-backed engine result, while each index-build case starts from an equivalent unindexed fixture.

use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use tempfile::{tempdir, TempDir};
use uqa_core::{Edge, Value, Vertex};
use uqa_engine::{Engine, HybridSearchParams};
use uqa_graph::{GraphStore, MemoryGraphStore, PathIndex, VertexMatch};
use uqa_sql::SQLParam;

const INDEX_ROWS: usize = 2_000;
const SEARCH_ROWS: usize = 4_000;
const GRAPH_VERTICES: u64 = 1_000;
const LIMIT: usize = 100;
const GRAPH: &str = "retrieval";

const CREATE_MESSAGES: &str = "CREATE TABLE messages (\
    id INTEGER PRIMARY KEY, \
    content TEXT, \
    kind TEXT, \
    embedding VECTOR(8))";
const CREATE_GIN: &str = "CREATE INDEX messages_content_gin ON messages USING gin (content)";
const CREATE_IVF: &str = "CREATE INDEX messages_embedding_ivf ON messages USING ivf (embedding) \
    WITH (lists = 16, probes = 4, train_threshold = 16)";
const TEXT_QUERY: &str = "SELECT id, _score FROM messages \
    WHERE bayesian_match(content, $1) \
    ORDER BY _score DESC LIMIT 100";
const VECTOR_QUERY: &str = "SELECT id, _score FROM messages \
    WHERE knn_match(embedding, $1, 100) \
    ORDER BY _score DESC LIMIT 100";

struct PersistentFixture {
    engine: Engine,
    _directory: TempDir,
}

fn vector_for_row(row: usize) -> Vec<f32> {
    let phase = (row % 16) as f32 / 16.0;
    vec![
        1.0 - phase,
        phase,
        (row % 7) as f32 / 7.0,
        (row % 5) as f32 / 5.0,
        0.25,
        0.5,
        0.75,
        1.0,
    ]
}

fn content_for_row(row: usize) -> String {
    if row.is_multiple_of(3) {
        format!("button search message {row} with repeated global conversation text")
    } else if row.is_multiple_of(5) {
        format!("ancient release note {row} with searchable button text")
    } else {
        format!("ordinary conversation row {row} with global search filler")
    }
}

fn persistent_fixture(rows: usize) -> PersistentFixture {
    let directory = tempdir().expect("temporary retrieval database directory");
    let engine =
        Engine::open(&directory.path().join("retrieval.sqlite3")).expect("open retrieval database");
    engine.sql(CREATE_MESSAGES, &[]).expect("create messages");
    engine
        .transaction(|engine| {
            for row in 1..=rows {
                engine.sql(
                    "INSERT INTO messages (id, content, kind, embedding) VALUES ($1, $2, $3, $4)",
                    &[
                        SQLParam::scalar(Value::Int(row as i64)),
                        SQLParam::scalar(Value::Str(content_for_row(row))),
                        SQLParam::scalar(Value::Str(
                            if row.is_multiple_of(11) {
                                "image"
                            } else {
                                "chat"
                            }
                            .to_string(),
                        )),
                        SQLParam::vector(vector_for_row(row)),
                    ],
                )?;
            }
            Ok(())
        })
        .expect("insert retrieval rows");
    PersistentFixture {
        engine,
        _directory: directory,
    }
}

fn indexed_persistent_fixture(rows: usize) -> PersistentFixture {
    let fixture = persistent_fixture(rows);
    fixture.engine.sql(CREATE_GIN, &[]).expect("create GIN");
    fixture.engine.sql(CREATE_IVF, &[]).expect("create IVF");
    fixture
}

fn graph_fixture(vertices: u64) -> MemoryGraphStore {
    let mut graph = MemoryGraphStore::new();
    graph.create_graph(GRAPH);
    for offset in 0..vertices {
        let mut vertex = Vertex::new(
            offset + 1,
            if offset.is_multiple_of(5) {
                "Company"
            } else {
                "Person"
            },
        );
        vertex
            .properties
            .insert("score".to_string(), Value::Int((offset % 100) as i64));
        graph
            .add_vertex(vertex, GRAPH)
            .expect("add benchmark vertex");
    }
    for source in 1..vertices {
        graph
            .add_edge(Edge::new(source, source, source + 1, "knows"), GRAPH)
            .expect("add benchmark edge");
    }
    graph
}

fn graph_path_sequences() -> Vec<Vec<String>> {
    (1..=3)
        .map(|depth| vec!["knows".to_string(); depth])
        .collect()
}

fn bench_index_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("retrieval_index_build");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    group.throughput(Throughput::Elements(INDEX_ROWS as u64));
    group.bench_function("text_gin_2k", |bencher| {
        bencher.iter_batched(
            || persistent_fixture(INDEX_ROWS),
            |fixture| black_box(fixture.engine.sql(CREATE_GIN, &[]).expect("create GIN")),
            BatchSize::LargeInput,
        );
    });
    group.bench_function("vector_ivf_2k", |bencher| {
        bencher.iter_batched(
            || persistent_fixture(INDEX_ROWS),
            |fixture| black_box(fixture.engine.sql(CREATE_IVF, &[]).expect("create IVF")),
            BatchSize::LargeInput,
        );
    });
    group.bench_function("hybrid_gin_ivf_2k", |bencher| {
        bencher.iter_batched(
            || persistent_fixture(INDEX_ROWS),
            |fixture| {
                fixture.engine.sql(CREATE_GIN, &[]).expect("create GIN");
                black_box(fixture.engine.sql(CREATE_IVF, &[]).expect("create IVF"))
            },
            BatchSize::LargeInput,
        );
    });

    group.throughput(Throughput::Elements(GRAPH_VERTICES));
    let sequences = graph_path_sequences();
    group.bench_function("graph_path_depth3_1k", |bencher| {
        bencher.iter_batched(
            || graph_fixture(GRAPH_VERTICES),
            |graph| black_box(PathIndex::build(&graph, GRAPH, &sequences)),
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

fn bench_search(c: &mut Criterion) {
    let fixture = indexed_persistent_fixture(SEARCH_ROWS);
    let text_query = SQLParam::scalar(Value::Str("button search".to_string()));
    let query_vector = vec![1.0, 0.0, 0.2, 0.4, 0.25, 0.5, 0.75, 1.0];
    let vector_query = SQLParam::vector(query_vector.clone());
    let graph = graph_fixture(GRAPH_VERTICES);

    let text_smoke = fixture
        .engine
        .sql(TEXT_QUERY, std::slice::from_ref(&text_query))
        .expect("text smoke");
    let vector_smoke = fixture
        .engine
        .sql(VECTOR_QUERY, std::slice::from_ref(&vector_query))
        .expect("vector smoke");
    let hybrid_smoke = fixture
        .engine
        .hybrid_search(&HybridSearchParams {
            table: "messages",
            text_field: "content",
            text_query: "button search",
            vector_field: "embedding",
            query_vector: query_vector.clone(),
            knn_pool: LIMIT,
            alpha: 0.5,
            top_k: LIMIT,
        })
        .expect("hybrid smoke search");
    let graph_smoke = VertexMatch::new(GRAPH)
        .label("Person")
        .execute(&graph)
        .expect("graph smoke search");
    assert!(!text_smoke.rows.is_empty());
    assert_eq!(vector_smoke.rows.len(), LIMIT);
    assert!(!hybrid_smoke.is_empty());
    assert_eq!(graph_smoke.len(), 800);

    let mut group = c.benchmark_group("retrieval_search");
    group.bench_function("text_bayesian_4k", |bencher| {
        bencher.iter(|| {
            let result = fixture
                .engine
                .sql(black_box(TEXT_QUERY), std::slice::from_ref(&text_query))
                .expect("text search");
            black_box(result.rows)
        });
    });
    group.bench_function("vector_ivf_top100_4k", |bencher| {
        bencher.iter(|| {
            let result = fixture
                .engine
                .sql(black_box(VECTOR_QUERY), std::slice::from_ref(&vector_query))
                .expect("vector search");
            black_box(result.rows)
        });
    });
    group.bench_function("graph_vertex_match_1k", |bencher| {
        bencher.iter(|| {
            let result = VertexMatch::new(GRAPH)
                .label(black_box("Person"))
                .execute(&graph)
                .expect("graph vertex match");
            black_box(result.len())
        });
    });
    group.bench_function("hybrid_text_vector_4k", |bencher| {
        bencher.iter(|| {
            let result = fixture.engine.hybrid_search(&HybridSearchParams {
                table: "messages",
                text_field: "content",
                text_query: "button search",
                vector_field: "embedding",
                query_vector: query_vector.clone(),
                knn_pool: LIMIT,
                alpha: 0.5,
                top_k: LIMIT,
            });
            black_box(result)
        });
    });
    group.finish();
}

fn benches(c: &mut Criterion) {
    bench_index_build(c);
    bench_search(c);
}

criterion_group!(retrieval_benches, benches);
criterion_main!(retrieval_benches);
