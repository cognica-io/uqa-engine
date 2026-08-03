//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Planner benchmarks for `DPccp` join enumeration on chain, star, clique,
//! and cycle
//! topologies, plus the greedy fallback path for larger relation sets.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_planner::{enumerate_dpccp, JoinGraph};

#[derive(Debug, Clone, Copy)]
enum Shape {
    Chain,
    Star,
    Clique,
    Cycle,
}

fn graph(shape: Shape, n: usize) -> JoinGraph {
    let mut graph = JoinGraph::new();
    for i in 0..n {
        graph
            .add_relation(format!("t{i}"), 1_000.0 + i as f64 * 100.0)
            .unwrap();
    }
    match shape {
        Shape::Chain => {
            for i in 0..n.saturating_sub(1) {
                graph.add_edge(i, i + 1, 0.01).unwrap();
            }
        }
        Shape::Star => {
            for i in 1..n {
                graph.add_edge(0, i, 0.01).unwrap();
            }
        }
        Shape::Clique => {
            for i in 0..n {
                for j in (i + 1)..n {
                    graph.add_edge(i, j, 0.01).unwrap();
                }
            }
        }
        Shape::Cycle => {
            for i in 0..n {
                graph.add_edge(i, (i + 1) % n, 0.01).unwrap();
            }
        }
    }
    graph
}

fn bench_dpccp_shapes(c: &mut Criterion) {
    let mut group = c.benchmark_group("planner_dpccp_topology");
    for (shape_name, shape, sizes) in [
        ("chain", Shape::Chain, &[3_usize, 5, 8, 10][..]),
        ("star", Shape::Star, &[3_usize, 5, 8, 10, 16][..]),
        ("clique", Shape::Clique, &[3_usize, 5, 8][..]),
        ("cycle", Shape::Cycle, &[3_usize, 5, 8, 10][..]),
    ] {
        for n in sizes {
            let g = graph(shape, *n);
            group.bench_with_input(BenchmarkId::new(shape_name, n), n, |bencher, _| {
                bencher.iter(|| {
                    let plan = enumerate_dpccp(black_box(&g));
                    black_box(plan.map(|p| p.cost))
                });
            });
        }
    }
    group.finish();
}

fn bench_dpccp_fixed_topology(c: &mut Criterion) {
    let mut group = c.benchmark_group("planner_dpccp_fixed_8");
    for (name, shape) in [
        ("chain_8", Shape::Chain),
        ("star_8", Shape::Star),
        ("clique_8", Shape::Clique),
        ("cycle_8", Shape::Cycle),
    ] {
        let g = graph(shape, 8);
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let plan = enumerate_dpccp(black_box(&g));
                black_box(plan.map(|p| p.cost))
            });
        });
    }
    group.finish();
}

fn bench_greedy_fallback(c: &mut Criterion) {
    let mut chain_group = c.benchmark_group("planner_greedy_chain");
    for n in [16_usize, 20, 30] {
        let g = graph(Shape::Chain, n);
        chain_group.bench_with_input(BenchmarkId::from_parameter(n), &n, |bencher, _| {
            bencher.iter(|| {
                let plan = enumerate_dpccp(black_box(&g));
                black_box(plan.map(|p| p.cost))
            });
        });
    }
    chain_group.finish();

    let mut star_group = c.benchmark_group("planner_greedy_star");
    for n in [20_usize, 30] {
        let g = graph(Shape::Star, n);
        star_group.bench_with_input(BenchmarkId::from_parameter(n), &n, |bencher, _| {
            bencher.iter(|| {
                let plan = enumerate_dpccp(black_box(&g));
                black_box(plan.map(|p| p.cost))
            });
        });
    }
    star_group.finish();
}

criterion_group!(
    benches,
    bench_dpccp_shapes,
    bench_dpccp_fixed_topology,
    bench_greedy_fallback
);
criterion_main!(benches);
