//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Planner benchmarks for `DPccp` join enumeration on chain, star, clique,
//! and cycle topologies, plus costed local access paths and the greedy
//! fallback path for larger relation sets.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_core::IndexStats;
use uqa_operators::OperatorTree;
use uqa_planner::cost_model::CostModel;
use uqa_planner::{
    enumerate_dpccp_with_cost_estimator, CardinalityEstimator, CostEstimator, JoinGraph,
    OperatorKind,
};

#[derive(Debug, Clone, Copy)]
enum Shape {
    Chain,
    Star,
    Clique,
    Cycle,
}

fn graph(shape: Shape, n: usize) -> JoinGraph {
    let mut graph = JoinGraph::new();
    let estimator = CostEstimator::default();
    for i in 0..n {
        let rows = 1_000.0 + i as f64 * 100.0;
        graph
            .add_relation_with_cost(
                format!("t{i}"),
                rows,
                estimator
                    .estimate_unary(OperatorKind::TableScan, rows)
                    .total(),
            )
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
                    let plan = enumerate_dpccp_with_cost_estimator(
                        black_box(&g),
                        CostEstimator::default(),
                    );
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
                let plan =
                    enumerate_dpccp_with_cost_estimator(black_box(&g), CostEstimator::default());
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
                let plan =
                    enumerate_dpccp_with_cost_estimator(black_box(&g), CostEstimator::default());
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
                let plan =
                    enumerate_dpccp_with_cost_estimator(black_box(&g), CostEstimator::default());
                black_box(plan.map(|p| p.cost))
            });
        });
    }
    star_group.finish();
}

fn bench_costed_access_path(c: &mut Criterion) {
    let estimator = CostEstimator::default();
    let mut index_stats = IndexStats::new(1_000_000);
    index_stats.dimensions = 128;
    let retrieval_tree = OperatorTree::KNN {
        query_vector: vec![0.0; 128],
        k: 3,
        field: "embedding".into(),
    };
    let retrieval_rows = CardinalityEstimator::new().estimate(&retrieval_tree, &index_stats);
    let retrieval_cost = CostModel::new().estimate(&retrieval_tree, &index_stats);
    let mut graph = JoinGraph::new();
    let retrieval = graph
        .add_relation_with_cost("retrieval", retrieval_rows, retrieval_cost)
        .unwrap();
    let mut previous = retrieval;
    for index in 1..8 {
        let rows = 1_000.0 * f64::from(index);
        let relation = graph
            .add_relation_with_cost(
                format!("table_{index}"),
                rows,
                estimator
                    .estimate_unary(OperatorKind::TableScan, rows)
                    .total(),
            )
            .unwrap();
        graph.add_edge(previous, relation, 0.01).unwrap();
        previous = relation;
    }
    c.bench_function("planner_dpccp_costed_access", |bencher| {
        bencher.iter(|| {
            let plan =
                enumerate_dpccp_with_cost_estimator(black_box(&graph), CostEstimator::default());
            black_box(plan.map(|plan| plan.cost))
        });
    });
}

criterion_group!(
    benches,
    bench_dpccp_shapes,
    bench_dpccp_fixed_topology,
    bench_greedy_fallback,
    bench_costed_access_path
);
criterion_main!(benches);
