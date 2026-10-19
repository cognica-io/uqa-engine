//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Repeated constant-pattern predicates over stored rows, including a non-regex control.

use criterion::{criterion_group, criterion_main, Criterion, SamplingMode};
use std::{hint::black_box, time::Duration};
use uqa_core::Value;
use uqa_engine::Engine;

fn regex_predicates(criterion: &mut Criterion) {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE regex_inputs AS SELECT CASE WHEN id % 2 = 0 THEN 'x' ELSE '9' END AS value FROM generate_series(1, 20000) AS input(id)",
            &[],
        )
        .unwrap();
    let mut group = criterion.benchmark_group("regex_predicates");
    group.sample_size(20);
    group.sampling_mode(SamplingMode::Linear);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    for (name, query) in [
        (
            "equality",
            "SELECT count(*) AS matched FROM regex_inputs WHERE value = 'x'",
        ),
        (
            "literal",
            "SELECT count(*) AS matched FROM regex_inputs WHERE value ~ 'x'",
        ),
        (
            "character_class",
            "SELECT count(*) AS matched FROM regex_inputs WHERE value ~ '[[:alpha:]]'",
        ),
    ] {
        let verify = || {
            let result = engine.sql(query, &[]).unwrap();
            assert_eq!(result.rows.len(), 1);
            assert_eq!(result.rows[0]["matched"], Value::Int(10_000));
            black_box(result)
        };
        verify();
        group.bench_function(name, |bench| bench.iter(&verify));
    }
    group.finish();
}

criterion_group!(benches, regex_predicates);
criterion_main!(benches);
