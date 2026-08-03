//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Same-process analytical comparison against `SQLite` and `DuckDB`.

#[path = "analytical_comparison/backends.rs"]
mod backends;
#[path = "analytical_comparison/fixture.rs"]
mod fixture;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use backends::Backends;
use fixture::Fixture;

fn configured_group<'a>(
    criterion: &'a mut Criterion,
    name: &str,
    fixture: &Fixture,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut group = criterion.benchmark_group(name);
    group.sample_size(fixture.manifest.criterion.sample_size);
    group.warm_up_time(fixture.warm_up());
    group.measurement_time(fixture.measurement());
    group.sampling_mode(fixture.sampling_mode());
    group
}

fn bench_external_analytical(c: &mut Criterion) {
    let fixture = Fixture::load();
    let backends = Backends::new(&fixture);
    backends.validate(&fixture);

    let mut q1 = configured_group(c, "analytical_external_q1", &fixture);
    q1.bench_function("uqa", |b| {
        b.iter(|| black_box(backends.uqa_q1(&fixture)));
    });
    q1.bench_function("sqlite", |b| {
        b.iter(|| black_box(backends.sqlite_q1(&fixture)));
    });
    q1.bench_function("duckdb", |b| {
        b.iter(|| black_box(backends.duckdb_q1(&fixture)));
    });
    q1.finish();

    let mut q6 = configured_group(c, "analytical_external_q6", &fixture);
    q6.bench_function("uqa", |b| {
        b.iter(|| black_box(backends.uqa_q6(&fixture)));
    });
    q6.bench_function("sqlite", |b| {
        b.iter(|| black_box(backends.sqlite_q6(&fixture)));
    });
    q6.bench_function("duckdb", |b| {
        b.iter(|| black_box(backends.duckdb_q6(&fixture)));
    });
    q6.finish();

    let mut scan = configured_group(c, "analytical_result_scan", &fixture);
    scan.bench_function("uqa_materialized", |b| {
        b.iter(|| black_box(backends.uqa_scan(&fixture)));
    });
    scan.bench_function("uqa_cursor", |b| {
        b.iter(|| black_box(backends.uqa_cursor_scan(&fixture)));
    });
    scan.bench_function("sqlite", |b| {
        b.iter(|| black_box(backends.sqlite_scan(&fixture)));
    });
    scan.bench_function("duckdb", |b| {
        b.iter(|| black_box(backends.duckdb_scan(&fixture)));
    });
    scan.finish();
}

criterion_group!(benches, bench_external_analytical);
criterion_main!(benches);
