//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end coverage for every physical query shape in `UnifiedPlan`.

use std::fmt::Write;
use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLParam;

const RELATIONAL_ROWS: usize = 2_000;
const RETRIEVAL_ROWS: usize = 2_000;

struct ReadCase {
    name: &'static str,
    sql: &'static str,
}

type WallTimeGroup<'a> = criterion::BenchmarkGroup<'a, criterion::measurement::WallTime>;

fn relational_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE facts (\
                id INTEGER PRIMARY KEY, \
                category TEXT, \
                value INTEGER, \
                width INTEGER)",
            &[],
        )
        .expect("create facts");
    engine
        .sql(
            "CREATE TABLE dimensions (\
                id INTEGER PRIMARY KEY, \
                category TEXT, \
                label TEXT)",
            &[],
        )
        .expect("create dimensions");

    let mut facts = String::from("INSERT INTO facts (id, category, value, width) VALUES ");
    for id in 1..=RELATIONAL_ROWS {
        if id > 1 {
            facts.push_str(", ");
        }
        let _ = write!(
            facts,
            "({id}, 'cat_{}', {}, {})",
            id % 20,
            id % 1_000,
            id % 4 + 1
        );
    }
    engine.sql(&facts, &[]).expect("insert facts");

    let mut dimensions = String::from("INSERT INTO dimensions (id, category, label) VALUES ");
    for id in 0..20 {
        if id > 0 {
            dimensions.push_str(", ");
        }
        let _ = write!(dimensions, "({id}, 'cat_{id}', 'label_{id}')");
    }
    engine.sql(&dimensions, &[]).expect("insert dimensions");
    engine
}

fn retrieval_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE documents (\
                id INTEGER PRIMARY KEY, \
                body TEXT, \
                kind TEXT, \
                embedding VECTOR(4))",
            &[],
        )
        .expect("create documents");

    let mut documents = String::from("INSERT INTO documents (id, body, kind, embedding) VALUES ");
    for id in 1..=RETRIEVAL_ROWS {
        if id > 1 {
            documents.push_str(", ");
        }
        let body = if id % 3 == 0 {
            "rust query engine benchmark"
        } else if id % 5 == 0 {
            "graph vector search benchmark"
        } else {
            "ordinary document payload"
        };
        let kind = if id % 2 == 0 { "chat" } else { "note" };
        let phase = id % 10;
        let _ = write!(
            documents,
            "({id}, '{body}', '{kind}', ARRAY[0.{phase}, 0.{}, 0.5, 1.0])",
            9 - phase
        );
    }
    engine.sql(&documents, &[]).expect("insert documents");
    engine
        .sql(
            "CREATE INDEX documents_body_gin ON documents USING gin (body)",
            &[],
        )
        .expect("create text index");
    engine
        .sql(
            "CREATE INDEX documents_embedding_ivf ON documents USING ivf (embedding) \
             WITH (lists = 16, probes = 4, train_threshold = 16)",
            &[],
        )
        .expect("create vector index");
    engine
}

fn mutation_engine() -> Engine {
    let engine = relational_engine();
    engine
        .sql(
            "CREATE TABLE sink (id INTEGER PRIMARY KEY, value INTEGER)",
            &[],
        )
        .expect("create sink");
    engine
        .sql(
            "CREATE TABLE deltas (id INTEGER PRIMARY KEY, amount INTEGER)",
            &[],
        )
        .expect("create deltas");
    engine
        .sql(
            "INSERT INTO deltas (id, amount) VALUES (1, 3), (2, 5), (3, 7)",
            &[],
        )
        .expect("insert deltas");
    engine
}

fn validate_read_cases(engine: &Engine, cases: &[ReadCase]) {
    for case in cases {
        let result = engine
            .sql(case.sql, &[])
            .unwrap_or_else(|error| panic!("query matrix case `{}` failed: {error}", case.name));
        assert!(
            !result.rows.is_empty(),
            "query matrix case `{}` returned no rows",
            case.name
        );
    }
}

fn configure_group(group: &mut WallTimeGroup<'_>) {
    group.sample_size(30);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
}

fn bench_relational_queries(c: &mut Criterion) {
    let engine = relational_engine();
    let cases = [
        ReadCase {
            name: "query_block_constant",
            sql: "SELECT 1 AS value",
        },
        ReadCase {
            name: "query_block_row",
            sql: "SELECT id, value FROM facts WHERE value BETWEEN 400 AND 600",
        },
        ReadCase {
            name: "query_block_aggregate",
            sql: "SELECT category, COUNT(*), SUM(value) FROM facts GROUP BY category",
        },
        ReadCase {
            name: "query_block_window",
            sql: "SELECT id, ROW_NUMBER() OVER (PARTITION BY category ORDER BY value) AS rn FROM facts",
        },
        ReadCase {
            name: "set_union",
            sql: "SELECT id FROM facts WHERE id <= 1000 UNION SELECT id FROM facts WHERE id > 500 AND id <= 1500",
        },
        ReadCase {
            name: "set_union_all",
            sql: "SELECT id FROM facts WHERE id <= 1000 UNION ALL SELECT id FROM facts WHERE id > 1000",
        },
        ReadCase {
            name: "set_intersect",
            sql: "SELECT id FROM facts WHERE id <= 1000 INTERSECT SELECT id FROM facts WHERE id > 500",
        },
        ReadCase {
            name: "set_except",
            sql: "SELECT id FROM facts WHERE id <= 1000 EXCEPT SELECT id FROM facts WHERE id > 500",
        },
        ReadCase {
            name: "standalone_values",
            sql: "VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'd')",
        },
        ReadCase {
            name: "source_values",
            sql: "SELECT id, label FROM (VALUES (1, 'a'), (2, 'b'), (3, 'c')) AS v(id, label)",
        },
        ReadCase {
            name: "source_function",
            sql: "SELECT generate_series FROM generate_series(1, 1000)",
        },
        ReadCase {
            name: "source_subquery",
            sql: "SELECT id FROM (SELECT id FROM facts WHERE value > 900) AS filtered",
        },
        ReadCase {
            name: "source_join",
            sql: "SELECT f.id, d.label FROM facts f JOIN dimensions d ON f.category = d.category",
        },
        ReadCase {
            name: "source_lateral",
            sql: "SELECT f.id, s.generate_series FROM facts f, LATERAL generate_series(1, f.width) AS s WHERE f.id <= 100",
        },
        ReadCase {
            name: "cte_non_recursive",
            sql: "WITH filtered AS (SELECT id, value FROM facts WHERE value > 900) SELECT COUNT(*) AS total FROM filtered",
        },
        ReadCase {
            name: "cte_recursive",
            sql: "WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM numbers WHERE n < 500) SELECT COUNT(*) AS total FROM numbers",
        },
        ReadCase {
            name: "grouping_sets",
            sql: "SELECT category, width, SUM(value) FROM facts GROUP BY GROUPING SETS ((category, width), (category), (width), ())",
        },
        ReadCase {
            name: "scalar_subquery",
            sql: "SELECT id FROM facts WHERE value > (SELECT AVG(value) FROM facts)",
        },
        ReadCase {
            name: "correlated_exists",
            sql: "SELECT f.id FROM facts f WHERE EXISTS (SELECT 1 FROM dimensions d WHERE d.category = f.category)",
        },
    ];
    validate_read_cases(&engine, &cases);

    let mut group = c.benchmark_group("unified_read");
    configure_group(&mut group);
    for case in cases {
        group.bench_function(case.name, |bencher| {
            bencher.iter(|| {
                let result = engine.sql(black_box(case.sql), &[]).expect("read query");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

fn bench_retrieval_queries(c: &mut Criterion) {
    let engine = retrieval_engine();
    let cases = [
        ReadCase {
            name: "operator_tree_text",
            sql: "SELECT id, _score FROM documents WHERE text_match(body, 'rust query') ORDER BY _score DESC LIMIT 100",
        },
        ReadCase {
            name: "operator_tree_bayesian",
            sql: "SELECT id, _score FROM documents WHERE bayesian_match(body, 'rust query') ORDER BY _score DESC LIMIT 100",
        },
        ReadCase {
            name: "operator_tree_vector",
            sql: "SELECT id, _score FROM documents WHERE knn_match(embedding, ARRAY[0.9, 0.1, 0.5, 1.0], 100) ORDER BY _score DESC LIMIT 100",
        },
        ReadCase {
            name: "hybrid_residual",
            sql: "SELECT id, _score FROM documents WHERE text_match(body, 'rust') AND kind = 'chat' ORDER BY _score DESC LIMIT 100",
        },
        ReadCase {
            name: "hybrid_fusion",
            sql: "SELECT id, _score FROM documents WHERE fuse_log_odds(bayesian_match(body, 'rust'), knn_match(embedding, ARRAY[0.9, 0.1, 0.5, 1.0], 100)) ORDER BY _score DESC LIMIT 100",
        },
    ];
    validate_read_cases(&engine, &cases);

    let mut group = c.benchmark_group("unified_retrieval");
    configure_group(&mut group);
    for case in cases {
        group.bench_function(case.name, |bencher| {
            bencher.iter(|| {
                let result = engine
                    .sql(black_box(case.sql), &[])
                    .expect("retrieval query");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

fn int_param(value: i64) -> SQLParam {
    SQLParam::scalar(Value::Int(value))
}

fn bench_insert_sources(group: &mut WallTimeGroup<'_>, engine: &Engine) {
    let mut insert_id = 100_000_i64;
    group.bench_function("insert_values", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut measured = Duration::ZERO;
            for _ in 0..iterations {
                insert_id += 1;
                let started = Instant::now();
                let result = engine
                    .sql(
                        black_box("INSERT INTO sink (id, value) VALUES ($1, $2)"),
                        &[int_param(insert_id), int_param(1)],
                    )
                    .expect("insert values");
                black_box(result.affected_rows);
                measured += started.elapsed();
                engine
                    .sql("DELETE FROM sink WHERE id = $1", &[int_param(insert_id)])
                    .expect("restore insert-values state");
            }
            measured
        });
    });

    let mut select_id = 200_000_i64;
    group.bench_function("insert_select", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut measured = Duration::ZERO;
            for _ in 0..iterations {
                select_id += 1;
                let started = Instant::now();
                let result = engine
                    .sql(
                        black_box(
                            "INSERT INTO sink (id, value) SELECT $1, value FROM facts WHERE id = 1",
                        ),
                        &[int_param(select_id)],
                    )
                    .expect("insert select");
                black_box(result.affected_rows);
                measured += started.elapsed();
                engine
                    .sql("DELETE FROM sink WHERE id = $1", &[int_param(select_id)])
                    .expect("restore insert-select state");
            }
            measured
        });
    });
}

fn bench_insert_conflict(group: &mut WallTimeGroup<'_>, engine: &Engine) {
    engine
        .sql("INSERT INTO sink (id, value) VALUES (1, 0)", &[])
        .expect("seed upsert target");
    group.bench_function("insert_on_conflict", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut measured = Duration::ZERO;
            for _ in 0..iterations {
                let started = Instant::now();
                let result = engine
                    .sql(
                        black_box(
                            "INSERT INTO sink (id, value) VALUES (1, $1) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value RETURNING id",
                        ),
                        &[int_param(5)],
                    )
                    .expect("insert on conflict");
                black_box(result.rows.len());
                measured += started.elapsed();
                engine
                    .sql("UPDATE sink SET value = 0 WHERE id = 1", &[])
                    .expect("restore upsert state");
            }
            measured
        });
    });
}

fn bench_updates(group: &mut WallTimeGroup<'_>, engine: &Engine) {
    group.bench_function("update_returning", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut measured = Duration::ZERO;
            for _ in 0..iterations {
                let started = Instant::now();
                let result = engine
                    .sql(
                        black_box(
                            "UPDATE facts SET value = value + 1 WHERE id = 1 RETURNING id, value",
                        ),
                        &[],
                    )
                    .expect("update returning");
                black_box(result.rows.len());
                measured += started.elapsed();
                engine
                    .sql("UPDATE facts SET value = 1 WHERE id = 1", &[])
                    .expect("restore update-returning state");
            }
            measured
        });
    });

    group.bench_function("update_from", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut measured = Duration::ZERO;
            for _ in 0..iterations {
                let started = Instant::now();
                let result = engine
                    .sql(
                        black_box(
                            "UPDATE facts AS f SET value = f.value + d.amount FROM deltas AS d WHERE f.id = d.id RETURNING f.id",
                        ),
                        &[],
                    )
                    .expect("update from");
                black_box(result.rows.len());
                measured += started.elapsed();
                engine
                    .sql("UPDATE facts SET value = id WHERE id IN (1, 2, 3)", &[])
                    .expect("restore update-from state");
            }
            measured
        });
    });
}

fn bench_delete_and_merge(group: &mut WallTimeGroup<'_>, engine: &Engine) {
    let mut delete_id = 300_000_i64;
    group.bench_function("delete_using", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut measured = Duration::ZERO;
            for _ in 0..iterations {
                delete_id += 1;
                engine
                    .sql(
                        "INSERT INTO sink (id, value) VALUES ($1, 1)",
                        &[int_param(delete_id)],
                    )
                    .expect("seed delete row");
                let started = Instant::now();
                let result = engine
                    .sql(
                        black_box(
                            "DELETE FROM sink AS s USING deltas AS d WHERE s.id = $1 AND d.id = 1 RETURNING s.id",
                        ),
                        &[int_param(delete_id)],
                    )
                    .expect("delete using");
                black_box(result.rows.len());
                measured += started.elapsed();
            }
            measured
        });
    });

    group.bench_function("merge_matched", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut measured = Duration::ZERO;
            for _ in 0..iterations {
                let started = Instant::now();
                let result = engine
                    .sql(
                        black_box(
                            "MERGE INTO facts AS f USING deltas AS d ON f.id = d.id WHEN MATCHED THEN UPDATE SET value = f.value + d.amount RETURNING f.id",
                        ),
                        &[],
                    )
                    .expect("merge matched");
                black_box(result.rows.len());
                measured += started.elapsed();
                engine
                    .sql("UPDATE facts SET value = id WHERE id IN (1, 2, 3)", &[])
                    .expect("restore merge state");
            }
            measured
        });
    });
}

fn bench_mutation_queries(c: &mut Criterion) {
    let engine = mutation_engine();
    let mut group = c.benchmark_group("unified_mutation");
    configure_group(&mut group);
    bench_insert_sources(&mut group, &engine);
    bench_insert_conflict(&mut group, &engine);
    bench_updates(&mut group, &engine);
    bench_delete_and_merge(&mut group, &engine);
    group.finish();
}

criterion_group!(
    benches,
    bench_relational_queries,
    bench_retrieval_queries,
    bench_mutation_queries
);
criterion_main!(benches);
