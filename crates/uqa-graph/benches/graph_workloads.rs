//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph benchmarks mirroring UQA `bench_graph.py`,
//! `bench_graph_advanced.py`, `bench_graph_centrality.py`,
//! `bench_named_graphs.py`, and the graph-store portion of
//! `bench_storage.py`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_core::{Edge, Value, Vertex};
use uqa_graph::cypher::parse_cypher;
use uqa_graph::{
    AggregationKind, BetweennessCentrality, Direction, EdgePattern, GMatch, GraphDelta,
    GraphEmbedding, GraphPattern, GraphStore, IncrementalPatternMatcher, MemoryGraphStore,
    MessagePassing, PageRank, PathIndex, RegularPathQuery, SubgraphIndex, TemporalFilter,
    TemporalTraverse, Traverse, VersionedGraphStore, VertexPattern, VertexPredicate,
    WeightedPathQueryOperator, HITS,
};
use uqa_operators::{ExecutionContext, Operator};

const GRAPH: &str = "bench";

fn build_graph(n: u64) -> MemoryGraphStore {
    let mut g = MemoryGraphStore::new();
    g.create_graph(GRAPH);
    for id in 0..n {
        let mut vertex = Vertex::new(id + 1, if id % 5 == 0 { "Company" } else { "Person" });
        vertex
            .properties
            .insert("score".to_string(), Value::Int((id % 100) as i64));
        vertex
            .properties
            .insert("group".to_string(), Value::Str(format!("g{}", id % 10)));
        g.add_vertex(vertex, GRAPH);
    }
    let mut edge_id = 1_u64;
    for id in 1..n {
        let label = if id % 3 == 0 { "works_at" } else { "knows" };
        let mut edge = Edge::new(edge_id, id, id + 1, label);
        edge.properties
            .insert("weight".to_string(), Value::Float((id % 10) as f64 / 10.0));
        edge.properties
            .insert("valid_from".to_string(), Value::Int(0));
        edge.properties
            .insert("valid_to".to_string(), Value::Int(1_000));
        g.add_edge(edge, GRAPH);
        edge_id += 1;
    }
    for id in 1..n.saturating_sub(10) {
        if id % 10 == 0 {
            g.add_edge(Edge::new(edge_id, id, id + 10, "knows"), GRAPH);
            edge_id += 1;
        }
    }
    g
}

fn bfs(store: &MemoryGraphStore, start: u64, depth: u32, label: Option<&str>) -> usize {
    let mut op = Traverse::new(start, GRAPH).max_hops(depth);
    if let Some(label) = label {
        op = op.label(label);
    }
    op.execute(store).inner().len()
}

fn person_knows_pattern() -> GraphPattern {
    GraphPattern::new()
        .add_vertex(VertexPattern::new("a").with(VertexPredicate::LabelEq("Person".into())))
        .add_vertex(VertexPattern::new("b").with(VertexPredicate::LabelEq("Person".into())))
        .add_edge(EdgePattern::new("a", "b").with_label("knows"))
}

fn bench_graph_store_and_traversal(c: &mut Criterion) {
    c.bench_function("graph_store_add_vertices_1000", |bencher| {
        bencher.iter(|| {
            let mut g = MemoryGraphStore::new();
            g.create_graph(GRAPH);
            for id in 0..1_000 {
                g.add_vertex(Vertex::new(id + 1, "Person"), GRAPH);
            }
            black_box(g.vertices_in_graph(GRAPH).len())
        });
    });

    c.bench_function("graph_store_add_edges_1000", |bencher| {
        bencher.iter(|| {
            let mut g = MemoryGraphStore::new();
            g.create_graph(GRAPH);
            for id in 0..1_001 {
                g.add_vertex(Vertex::new(id + 1, "Person"), GRAPH);
            }
            for id in 0..1_000 {
                g.add_edge(Edge::new(id + 1, id + 1, id + 2, "knows"), GRAPH);
            }
            black_box(g.edges_in_graph(GRAPH).len())
        });
    });

    let store = build_graph(1_000);
    c.bench_function("graph_store_neighbors", |bencher| {
        bencher.iter(|| {
            let result = store.neighbors(black_box(1), None, Direction::Out, GRAPH);
            black_box(result.len())
        });
    });
    c.bench_function("graph_store_neighbors_with_label", |bencher| {
        bencher.iter(|| {
            let result = store.neighbors(black_box(1), Some("knows"), Direction::Out, GRAPH);
            black_box(result.len())
        });
    });

    let mut depth_group = c.benchmark_group("graph_bfs_depth");
    for depth in [1_u32, 2, 3] {
        depth_group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |bencher, d| {
            bencher.iter(|| black_box(bfs(black_box(&store), black_box(1), black_box(*d), None)));
        });
    }
    depth_group.finish();
    c.bench_function("graph_bfs_with_label", |bencher| {
        bencher.iter(|| {
            black_box(bfs(
                black_box(&store),
                black_box(1),
                black_box(2),
                Some("knows"),
            ))
        });
    });
    c.bench_function("graph_vertices_by_label", |bencher| {
        bencher.iter(|| {
            let result = store.vertices_by_label(black_box("Person"), GRAPH);
            black_box(result.len())
        });
    });
}

fn bench_pattern_rpq_cypher(c: &mut Criterion) {
    let store = build_graph(1_000);
    let pattern = person_knows_pattern();
    c.bench_function("graph_pattern_single_edge", |bencher| {
        bencher.iter(|| {
            let result = GMatch::new(black_box(pattern.clone()), GRAPH).execute(black_box(&store));
            black_box(result.inner().len())
        });
    });
    c.bench_function("graph_pattern_labeled_edge", |bencher| {
        bencher.iter(|| {
            let result = GMatch::new(black_box(pattern.clone()), GRAPH).execute(black_box(&store));
            black_box(result.inner().len())
        });
    });

    let mut parse_group = c.benchmark_group("graph_rpq_parse");
    for expr in [
        "knows",
        "knows/works_at",
        "knows|works_at|located_in",
        "knows*",
        "(knows|works_at)*/located_in",
        "knows{2,4}",
    ] {
        parse_group.bench_function(expr, |bencher| {
            bencher.iter(|| black_box(uqa_graph::parse_rpq(black_box(expr)).expect("rpq")));
        });
    }
    parse_group.finish();

    c.bench_function("graph_rpq_bounded_execute", |bencher| {
        let expr = uqa_graph::parse_rpq("knows{2,4}").expect("bounded rpq");
        bencher.iter(|| {
            let result = RegularPathQuery::new(black_box(expr.clone()), GRAPH)
                .from_vertex(1)
                .execute(black_box(&store));
            black_box(result.inner().len())
        });
    });

    let mut cypher_group = c.benchmark_group("graph_cypher_compile");
    for query in [
        "MATCH (a)-[:knows]->(b) RETURN a, b",
        "MATCH (a)-[:knows*1..3]->(b) RETURN b",
        "MATCH (a:Person)-[:knows]->(b) WHERE b.score > 10 RETURN b",
    ] {
        cypher_group.bench_function(query, |bencher| {
            bencher.iter(|| black_box(parse_cypher(black_box(query)).expect("cypher")));
        });
    }
    cypher_group.finish();
}

fn bench_graph_path_index(c: &mut Criterion) {
    let store = build_graph(1_000);
    let mut path_group = c.benchmark_group("graph_path_index_build");
    for depth in [1_usize, 2, 3] {
        let sequences: Vec<Vec<String>> =
            (1..=depth).map(|d| vec!["knows".to_string(); d]).collect();
        path_group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |bencher, _| {
            bencher.iter(|| {
                let idx = PathIndex::build(black_box(&store), GRAPH, black_box(&sequences));
                black_box(idx.indexed_paths().len())
            });
        });
    }
    path_group.finish();

    let idx = PathIndex::build(
        &store,
        GRAPH,
        &[
            vec!["knows".to_string()],
            vec!["knows".to_string(), "knows".to_string()],
        ],
    );
    c.bench_function("graph_path_index_lookup", |bencher| {
        let seq = vec!["knows".to_string()];
        bencher.iter(|| black_box(idx.lookup(black_box(&seq)).map(BTreeSet::len)));
    });

    let pattern = person_knows_pattern();
    c.bench_function("graph_subgraph_index_build", |bencher| {
        bencher.iter(|| {
            let index = SubgraphIndex::build(
                black_box(&store),
                black_box(std::slice::from_ref(&pattern)),
                GRAPH,
            );
            black_box(index.indexed_patterns().len())
        });
    });

    let subgraph_index = SubgraphIndex::build(&store, std::slice::from_ref(&pattern), GRAPH);
    c.bench_function("graph_subgraph_index_lookup", |bencher| {
        bencher.iter(|| {
            black_box(
                subgraph_index
                    .lookup(black_box(&pattern))
                    .map(BTreeSet::len),
            )
        });
    });

    c.bench_function("graph_cached_pattern_match", |bencher| {
        bencher.iter(|| {
            let cached = subgraph_index.lookup(black_box(&pattern)).map_or_else(
                || {
                    GMatch::new(black_box(pattern.clone()), GRAPH)
                        .execute(black_box(&store))
                        .inner()
                        .len()
                },
                BTreeSet::len,
            );
            black_box(cached)
        });
    });
}

fn bench_graph_delta(c: &mut Criterion) {
    let mut delta_group = c.benchmark_group("graph_delta_apply");
    for size in [10_u64, 100, 1_000] {
        delta_group.bench_with_input(BenchmarkId::from_parameter(size), &size, |bencher, size| {
            bencher.iter(|| {
                let mut base = MemoryGraphStore::new();
                base.create_graph(GRAPH);
                let mut versioned = VersionedGraphStore::new(&mut base, GRAPH);
                let mut delta = GraphDelta::new();
                for id in 0..*size {
                    delta.add_vertex(Vertex::new(id + 1, "Person"));
                }
                let version = versioned.apply(delta);
                black_box(version)
            });
        });
    }
    delta_group.finish();

    let mut rollback_group = c.benchmark_group("graph_delta_rollback");
    for depth in [1_u64, 5, 10] {
        rollback_group.bench_with_input(
            BenchmarkId::from_parameter(depth),
            &depth,
            |bencher, depth| {
                bencher.iter(|| {
                    let mut base = MemoryGraphStore::new();
                    base.create_graph(GRAPH);
                    let mut versioned = VersionedGraphStore::new(&mut base, GRAPH);
                    for version in 0..*depth {
                        let mut delta = GraphDelta::new();
                        delta.add_vertex(Vertex::new(version + 1, "Person"));
                        versioned.apply(delta);
                    }
                    versioned.rollback(0).expect("rollback");
                    black_box(versioned.version())
                });
            },
        );
    }
    rollback_group.finish();
}

fn bench_graph_temporal_message_embedding(c: &mut Criterion) {
    let store = build_graph(1_000);
    let mut temporal_group = c.benchmark_group("graph_temporal_traverse");
    for depth in [1_u32, 2, 3] {
        temporal_group.bench_with_input(
            BenchmarkId::from_parameter(depth),
            &depth,
            |bencher, depth| {
                bencher.iter(|| {
                    let result = TemporalTraverse::new(1, GRAPH)
                        .label("knows")
                        .max_hops(*depth)
                        .filter(TemporalFilter::Timestamp(100.0))
                        .execute(black_box(&store));
                    black_box(result.inner().len())
                });
            },
        );
    }
    temporal_group.finish();

    let mut message_group = c.benchmark_group("graph_message_passing");
    for layers in [1_u32, 2, 3] {
        message_group.bench_with_input(
            BenchmarkId::from_parameter(layers),
            &layers,
            |bencher, layers| {
                bencher.iter(|| {
                    let result = MessagePassing::new(GRAPH)
                        .property_name("score")
                        .k_layers(*layers)
                        .aggregation(AggregationKind::Mean)
                        .execute(black_box(&store));
                    black_box(result.inner().len())
                });
            },
        );
    }
    message_group.finish();

    let mut embedding_group = c.benchmark_group("graph_embedding_dimensions");
    for dimensions in [8_usize, 16, 32] {
        embedding_group.bench_with_input(
            BenchmarkId::from_parameter(dimensions),
            &dimensions,
            |bencher, dimensions| {
                bencher.iter(|| {
                    let result = GraphEmbedding::new(GRAPH)
                        .dimensions(*dimensions)
                        .k_layers(2)
                        .execute(black_box(&store));
                    black_box(result.inner().len())
                });
            },
        );
    }
    embedding_group.finish();
}

fn edge_weight(edge: &Edge) -> f64 {
    match edge.properties.get("weight") {
        Some(Value::Float(weight)) => *weight,
        Some(Value::Int(weight)) => *weight as f64,
        _ => 1.0,
    }
}

fn weighted_two_hop_max(store: &MemoryGraphStore, start: u64) -> BTreeMap<u64, f64> {
    let mut out = BTreeMap::new();
    for first_id in store.out_edge_ids(start, GRAPH) {
        let Some(first) = store.get_edge(first_id) else {
            continue;
        };
        if first.label != "knows" {
            continue;
        }
        let first_weight = edge_weight(first);
        for second_id in store.out_edge_ids(first.target_id, GRAPH) {
            let Some(second) = store.get_edge(second_id) else {
                continue;
            };
            if second.label != "knows" {
                continue;
            }
            let path_weight = first_weight.max(edge_weight(second));
            out.entry(second.target_id)
                .and_modify(|stored| {
                    if path_weight > *stored {
                        *stored = path_weight;
                    }
                })
                .or_insert(path_weight);
        }
    }
    out
}

fn bench_centrality_and_weighted_path(c: &mut Criterion) {
    let store = build_graph(500);
    c.bench_function("graph_pagerank_default", |bencher| {
        bencher.iter(|| {
            black_box(
                PageRank::new(GRAPH)
                    .execute(black_box(&store))
                    .inner()
                    .len(),
            )
        });
    });
    c.bench_function("graph_pagerank_high_damping", |bencher| {
        bencher.iter(|| {
            black_box(
                PageRank::new(GRAPH)
                    .damping(0.95)
                    .max_iterations(50)
                    .execute(black_box(&store))
                    .inner()
                    .len(),
            )
        });
    });
    c.bench_function("graph_pagerank_low_iterations", |bencher| {
        bencher.iter(|| {
            black_box(
                PageRank::new(GRAPH)
                    .max_iterations(10)
                    .execute(black_box(&store))
                    .inner()
                    .len(),
            )
        });
    });
    c.bench_function("graph_hits_default", |bencher| {
        bencher.iter(|| black_box(HITS::new(GRAPH).execute(black_box(&store)).inner().len()));
    });
    c.bench_function("graph_hits_low_iterations", |bencher| {
        bencher.iter(|| {
            black_box(
                HITS::new(GRAPH)
                    .max_iterations(10)
                    .execute(black_box(&store))
                    .inner()
                    .len(),
            )
        });
    });
    c.bench_function("graph_betweenness", |bencher| {
        bencher.iter(|| {
            black_box(
                BetweennessCentrality::new(GRAPH)
                    .execute(black_box(&store))
                    .inner()
                    .len(),
            )
        });
    });

    let graph_store = Arc::new(build_graph(500));
    let expr = uqa_graph::parse_rpq("knows{2,4}").expect("bounded rpq");
    let weighted = WeightedPathQueryOperator::new(expr.clone(), Arc::clone(&graph_store), GRAPH)
        .from_vertex(1)
        .with_score(0.8);
    let weighted_predicate = WeightedPathQueryOperator::new(expr, Arc::clone(&graph_store), GRAPH)
        .from_vertex(1)
        .with_predicate_selectivity(0.5);
    let ctx = ExecutionContext::new();
    c.bench_function("graph_weighted_path_sum", |bencher| {
        bencher.iter(|| black_box(weighted.execute(black_box(&ctx)).len()));
    });
    let max_store = build_graph(500);
    c.bench_function("graph_weighted_path_max", |bencher| {
        bencher.iter(|| black_box(weighted_two_hop_max(black_box(&max_store), black_box(1)).len()));
    });
    c.bench_function("graph_weighted_path_with_predicate", |bencher| {
        bencher.iter(|| black_box(weighted_predicate.execute(black_box(&ctx)).len()));
    });
}

fn bench_named_graphs(c: &mut Criterion) {
    c.bench_function("named_graph_create_100_graphs", |bencher| {
        bencher.iter(|| {
            let mut g = MemoryGraphStore::new();
            for i in 0..100 {
                g.create_graph(&format!("g{i}"));
            }
            black_box(g.graph_names().len())
        });
    });

    let mut store = build_graph(1_000);
    store.copy_graph(GRAPH, "copy");
    c.bench_function("named_graph_traverse_1hop", |bencher| {
        bencher.iter(|| {
            black_box(bfs(
                black_box(&store),
                black_box(1),
                black_box(1),
                Some("knows"),
            ))
        });
    });
    c.bench_function("named_graph_traverse_3hop", |bencher| {
        bencher.iter(|| {
            black_box(bfs(
                black_box(&store),
                black_box(1),
                black_box(3),
                Some("knows"),
            ))
        });
    });
    c.bench_function("named_graph_triangle_pattern", |bencher| {
        let pattern = GraphPattern::new()
            .add_vertex(VertexPattern::new("a"))
            .add_vertex(VertexPattern::new("b"))
            .add_vertex(VertexPattern::new("c"))
            .add_edge(EdgePattern::new("a", "b").with_label("knows"))
            .add_edge(EdgePattern::new("b", "c").with_label("knows"));
        bencher.iter(|| {
            let result = GMatch::new(black_box(pattern.clone()), GRAPH).execute(black_box(&store));
            black_box(result.inner().len())
        });
    });
    c.bench_function("named_graph_rpq_kleene", |bencher| {
        let expr = uqa_graph::parse_rpq("knows*").expect("rpq");
        bencher.iter(|| {
            let result = RegularPathQuery::new(black_box(expr.clone()), GRAPH)
                .from_vertex(1)
                .execute(black_box(&store));
            black_box(result.inner().len())
        });
    });
    c.bench_function("named_graph_union_graphs", |bencher| {
        bencher.iter(|| {
            let mut g = build_graph(500);
            g.copy_graph(GRAPH, "copy");
            g.union_graphs(GRAPH, "copy", "unioned");
            black_box(g.vertices_in_graph("unioned").len())
        });
    });
    c.bench_function("named_graph_intersect_graphs", |bencher| {
        bencher.iter(|| {
            let mut g = build_graph(500);
            g.copy_graph(GRAPH, "copy");
            g.intersect_graphs(GRAPH, "copy", "intersected");
            black_box(g.vertices_in_graph("intersected").len())
        });
    });
    c.bench_function("named_graph_property_index_build", |bencher| {
        bencher.iter(|| {
            let mut index: BTreeMap<String, Vec<u64>> = BTreeMap::new();
            for vertex in store.vertices_in_graph(GRAPH) {
                if let Some(Value::Str(group)) = vertex.properties.get("group") {
                    index
                        .entry(group.clone())
                        .or_default()
                        .push(vertex.vertex_id);
                }
            }
            black_box(index.len())
        });
    });
    c.bench_function("named_graph_isolation", |bencher| {
        bencher.iter(|| {
            let mut g = MemoryGraphStore::new();
            g.create_graph("a");
            g.create_graph("b");
            g.add_vertex(Vertex::new(1, "Person"), "a");
            g.add_vertex(Vertex::new(1, "Person"), "b");
            black_box(g.vertex_graphs(1).len())
        });
    });
}

fn bench_incremental_match(c: &mut Criterion) {
    c.bench_function("graph_incremental_add_edge", |bencher| {
        let mut store = build_graph(200);
        let pattern = person_knows_pattern();
        let mut matcher = IncrementalPatternMatcher::new(pattern, GRAPH);
        matcher.seed(&store);
        store.add_vertex(Vertex::new(500, "Person"), GRAPH);
        store.add_edge(Edge::new(10_000, 200, 500, "knows"), GRAPH);
        let mut delta = GraphDelta::new();
        delta.add_vertex(Vertex::new(500, "Person"));
        delta.add_edge(Edge::new(10_000, 200, 500, "knows"));
        bencher.iter(|| {
            let result = matcher.update(&store, &delta);
            black_box(result.len())
        });
    });

    c.bench_function("graph_incremental_remove_vertex", |bencher| {
        let mut store = build_graph(200);
        let pattern = person_knows_pattern();
        let mut matcher = IncrementalPatternMatcher::new(pattern, GRAPH);
        matcher.seed(&store);
        store.remove_vertex(2, GRAPH);
        let mut delta = GraphDelta::new();
        delta.remove_vertex(2);
        bencher.iter(|| {
            let result = matcher.update(&store, &delta);
            black_box(result.len())
        });
    });
}

criterion_group!(
    benches,
    bench_graph_store_and_traversal,
    bench_pattern_rpq_cypher,
    bench_graph_path_index,
    bench_graph_delta,
    bench_graph_temporal_message_embedding,
    bench_centrality_and_weighted_path,
    bench_named_graphs,
    bench_incremental_match
);
criterion_main!(benches);
