//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Spatial index benchmark: radius search over 100k synthetic points
//! distributed deterministically across the globe.
//!
//! Run with `cargo bench -p uqa-storage --bench spatial`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_storage::spatial_index::{MemorySpatialIndex, SpatialIndex};

const N: u64 = 100_000;

fn build_index() -> MemorySpatialIndex {
    let mut idx = MemorySpatialIndex::new("location");
    // Sweep a deterministic lat/lon grid over the planet.
    for i in 0..N {
        let lat = (i % 180) as f64 - 90.0;
        let lon = ((i / 180) % 360) as f64 - 180.0;
        idx.add(i, lon, lat);
    }
    idx
}

fn bench_radius_5km(c: &mut Criterion) {
    let idx = build_index();
    c.bench_function("spatial_radius_5km_100k", |bencher| {
        bencher.iter(|| {
            let pl = idx.search_within(black_box(0.0), black_box(0.0), black_box(5_000.0));
            black_box(pl.len())
        });
    });
}

fn bench_radius_500km(c: &mut Criterion) {
    let idx = build_index();
    c.bench_function("spatial_radius_500km_100k", |bencher| {
        bencher.iter(|| {
            let pl = idx.search_within(black_box(0.0), black_box(0.0), black_box(500_000.0));
            black_box(pl.len())
        });
    });
}

criterion_group!(benches, bench_radius_5km, bench_radius_500km);
criterion_main!(benches);
