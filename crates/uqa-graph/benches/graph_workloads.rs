//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph benchmarks mirroring UQA `bench_graph.py`,
//! `bench_graph_advanced.py`, `bench_graph_centrality.py`,
//! `bench_named_graphs.py`, and the graph-store portion of
//! `bench_storage.py`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use uqa_core::{Edge, Value, Vertex};
use uqa_graph::cypher::parse_cypher;
use uqa_graph::{
    AggregationKind, BetweennessCentrality, Direction, EdgePattern, GMatch, GraphDelta,
    GraphEmbedding, GraphPattern, GraphStore, IncrementalPatternMatcher, MemoryGraphStore,
    MessagePassing, PageRank, PathIndex, RegularPathQuery, SubgraphIndex, TemporalFilter,
    TemporalTraverse, Traverse, VersionedGraphStore, VertexMatch, VertexPattern, VertexPredicate,
    VertexPropertyIndex, WeightedPathQueryOperator, HITS,
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
        g.add_vertex(vertex, GRAPH).unwrap();
    }
    let mut edge_id = 1_u64;
    for id in 1..n {
        let label = if id % 3 == 0 { "works_at" } else { "knows" };
        let mut edge = Edge::new(edge_id, id, id + 1, label);
        edge.properties
            .insert("weight".to_string(), Value::Float((id % 10) as f64 / 10.0));
        let valid_from = (id % 50) as i64;
        edge.properties
            .insert("valid_from".to_string(), Value::Int(valid_from));
        edge.properties
            .insert("valid_to".to_string(), Value::Int(valid_from + 50));
        g.add_edge(edge, GRAPH).unwrap();
        edge_id += 1;
    }
    for id in 1..n.saturating_sub(10) {
        if id % 10 == 0 {
            g.add_edge(Edge::new(edge_id, id, id + 10, "knows"), GRAPH)
                .unwrap();
            edge_id += 1;
        }
    }
    g
}

fn build_named_chain(n: u64) -> MemoryGraphStore {
    let mut store = MemoryGraphStore::new();
    store.create_graph(GRAPH);
    for id in 1..=n {
        let mut vertex = Vertex::new(id, "node");
        vertex
            .properties
            .insert("val".to_string(), Value::Int(id as i64));
        store.add_vertex(vertex, GRAPH).unwrap();
    }
    for id in 1..n {
        store
            .add_edge(Edge::new(id, id, id + 1, "next"), GRAPH)
            .unwrap();
    }
    store
}

fn build_overlapping_graphs() -> MemoryGraphStore {
    let mut store = MemoryGraphStore::new();
    store.create_graph("alpha");
    store.create_graph("beta");
    for id in 1..=750 {
        let mut vertex = Vertex::new(id, "node");
        vertex
            .properties
            .insert("val".to_string(), Value::Int(id as i64));
        if id <= 500 {
            store.add_vertex(vertex.clone(), "alpha").unwrap();
        }
        if id >= 251 {
            store.add_vertex(vertex, "beta").unwrap();
        }
    }
    for id in 1..500 {
        store
            .add_edge(Edge::new(id, id, id + 1, "link"), "alpha")
            .unwrap();
    }
    for id in 251..750 {
        store
            .add_edge(Edge::new(1_000 + id, id, id + 1, "link"), "beta")
            .unwrap();
    }
    store
}

fn bfs(store: &MemoryGraphStore, start: u64, depth: u32, label: Option<&str>) -> usize {
    let mut op = Traverse::new(start, GRAPH).max_hops(depth);
    if let Some(label) = label {
        op = op.label(label);
    }
    op.execute(store)
        .expect("traversal benchmark")
        .inner()
        .len()
}

fn direct_bfs(
    store: &MemoryGraphStore,
    start: u64,
    depth: u32,
    label: Option<&str>,
    graph: &str,
) -> usize {
    let mut visited = BTreeSet::from([start]);
    let mut frontier = VecDeque::from([start]);
    for _ in 0..depth {
        let mut next_frontier = VecDeque::new();
        while let Some(vertex) = frontier.pop_front() {
            for neighbor in store
                .neighbors(vertex, label, Direction::Out, graph)
                .expect("direct traversal benchmark")
            {
                if visited.insert(neighbor) {
                    next_frontier.push_back(neighbor);
                }
            }
        }
        frontier = next_frontier;
        if frontier.is_empty() {
            break;
        }
    }
    visited.len()
}

fn unindexed_path_pair_count(store: &MemoryGraphStore, labels: &[String]) -> usize {
    let starts = store
        .vertex_ids_in_graph(GRAPH)
        .expect("path benchmark graph");
    let mut pairs = BTreeSet::new();
    for start in starts {
        let mut current = BTreeSet::from([start]);
        for label in labels {
            let mut next = BTreeSet::new();
            for vertex in current {
                for edge_id in store
                    .out_edge_ids(vertex, GRAPH)
                    .expect("path benchmark graph")
                {
                    let edge = store.get_edge(edge_id).expect("indexed graph edge");
                    if edge.label == *label {
                        next.insert(edge.target_id);
                    }
                }
            }
            current = next;
            if current.is_empty() {
                break;
            }
        }
        pairs.extend(current.into_iter().map(|end| (start, end)));
    }
    pairs.len()
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
                g.add_vertex(Vertex::new(id + 1, "Person"), GRAPH).unwrap();
            }
            black_box(g.vertices_in_graph(GRAPH).expect("benchmark graph").len())
        });
    });

    c.bench_function("graph_store_add_edges_1000", |bencher| {
        bencher.iter(|| {
            let mut g = MemoryGraphStore::new();
            g.create_graph(GRAPH);
            for id in 0..1_001 {
                g.add_vertex(Vertex::new(id + 1, "Person"), GRAPH).unwrap();
            }
            for id in 0..1_000 {
                g.add_edge(Edge::new(id + 1, id + 1, id + 2, "knows"), GRAPH)
                    .unwrap();
            }
            black_box(g.edges_in_graph(GRAPH).expect("benchmark graph").len())
        });
    });

    let store = build_graph(1_000);
    c.bench_function("graph_store_neighbors", |bencher| {
        bencher.iter(|| {
            let result = store.neighbors(black_box(1), None, Direction::Out, GRAPH);
            black_box(result.expect("benchmark neighbors").len())
        });
    });
    c.bench_function("graph_store_in_neighbors", |bencher| {
        bencher.iter(|| {
            let result = store.neighbors(black_box(500), None, Direction::In, GRAPH);
            black_box(result.expect("benchmark inbound neighbors").len())
        });
    });
    c.bench_function("graph_store_neighbors_with_label", |bencher| {
        bencher.iter(|| {
            let result = store.neighbors(black_box(1), Some("knows"), Direction::Out, GRAPH);
            black_box(result.expect("benchmark neighbors").len())
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
            black_box(result.expect("benchmark label lookup").len())
        });
    });
    c.bench_function("graph_vertex_match_label", |bencher| {
        bencher.iter(|| {
            let result = VertexMatch::new(GRAPH)
                .label(black_box("Person"))
                .execute(&store)
                .unwrap();
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
            black_box(result.expect("pattern benchmark").inner().len())
        });
    });
    c.bench_function("graph_pattern_labeled_edge", |bencher| {
        bencher.iter(|| {
            let result = GMatch::new(black_box(pattern.clone()), GRAPH).execute(black_box(&store));
            black_box(result.expect("pattern benchmark").inner().len())
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
            black_box(result.expect("RPQ benchmark").inner().len())
        });
    });

    let bounded = uqa_graph::parse_rpq("knows{1,2}").expect("bounded rpq");
    let kleene = uqa_graph::parse_rpq("knows*").expect("kleene rpq");
    let mut execute_group = c.benchmark_group("graph_rpq_bounded_vs_kleene");
    for (name, expr) in [("bounded", bounded), ("kleene", kleene)] {
        execute_group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = RegularPathQuery::new(black_box(expr.clone()), GRAPH)
                    .from_vertex(1)
                    .execute(black_box(&store));
                black_box(result.expect("RPQ comparison benchmark").inner().len())
            });
        });
    }
    execute_group.finish();

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
                black_box(idx.expect("path index build").indexed_paths().len())
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
    )
    .expect("path index build");
    c.bench_function("graph_path_index_lookup", |bencher| {
        let seq = vec!["knows".to_string()];
        bencher.iter(|| black_box(idx.lookup(black_box(&seq)).map(BTreeSet::len)));
    });

    let indexed_sequence = vec!["knows".to_string(), "knows".to_string()];
    let indexed_count = idx
        .lookup(&indexed_sequence)
        .expect("indexed sequence")
        .len();
    assert_eq!(
        indexed_count,
        unindexed_path_pair_count(&store, &indexed_sequence),
        "indexed and unindexed path evaluation must have the same carrier"
    );
    let mut comparison_group = c.benchmark_group("graph_rpq_indexed_vs_unindexed");
    comparison_group.bench_function("indexed", |bencher| {
        bencher.iter(|| {
            black_box(
                idx.lookup(black_box(&indexed_sequence))
                    .expect("indexed sequence")
                    .len(),
            )
        });
    });
    comparison_group.bench_function("unindexed", |bencher| {
        bencher.iter(|| {
            black_box(unindexed_path_pair_count(
                black_box(&store),
                black_box(&indexed_sequence),
            ))
        });
    });
    comparison_group.finish();

    let pattern = person_knows_pattern();
    c.bench_function("graph_subgraph_index_build", |bencher| {
        bencher.iter(|| {
            let index = SubgraphIndex::build(
                black_box(&store),
                black_box(std::slice::from_ref(&pattern)),
                GRAPH,
            );
            black_box(
                index
                    .expect("subgraph index build")
                    .indexed_patterns()
                    .len(),
            )
        });
    });

    let subgraph_index = SubgraphIndex::build(&store, std::slice::from_ref(&pattern), GRAPH)
        .expect("subgraph index build");
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
                        .expect("pattern benchmark")
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
                let version = versioned.apply(delta).unwrap();
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
                        versioned.apply(delta).unwrap();
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
                    black_box(result.expect("temporal traversal benchmark").inner().len())
                });
            },
        );
    }
    temporal_group.finish();

    let mut temporal_filter_group = c.benchmark_group("graph_temporal_filter_vs_unfiltered");
    for (name, filter) in [
        ("timestamp", TemporalFilter::Timestamp(25.0)),
        ("unfiltered", TemporalFilter::Any),
    ] {
        temporal_filter_group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = TemporalTraverse::new(1, GRAPH)
                    .label("knows")
                    .max_hops(2)
                    .filter(filter)
                    .execute(black_box(&store));
                black_box(result.expect("temporal filter benchmark").inner().len())
            });
        });
    }
    temporal_filter_group.finish();

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
                    black_box(result.expect("message-passing benchmark").inner().len())
                });
            },
        );
    }
    message_group.finish();

    let mut aggregation_group = c.benchmark_group("graph_message_passing_aggregation");
    for (name, aggregation) in [
        ("mean", AggregationKind::Mean),
        ("sum", AggregationKind::Sum),
        ("max", AggregationKind::Max),
    ] {
        aggregation_group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = MessagePassing::new(GRAPH)
                    .property_name("score")
                    .k_layers(2)
                    .aggregation(aggregation)
                    .execute(black_box(&store));
                black_box(result.expect("aggregation benchmark").inner().len())
            });
        });
    }
    aggregation_group.finish();

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
                    black_box(result.expect("embedding benchmark").inner().len())
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
    for first_id in store.out_edge_ids(start, GRAPH).expect("benchmark graph") {
        let Some(first) = store.get_edge(first_id) else {
            continue;
        };
        if first.label != "knows" {
            continue;
        }
        let first_weight = edge_weight(first);
        for second_id in store
            .out_edge_ids(first.target_id, GRAPH)
            .expect("benchmark graph")
        {
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

fn bench_centrality(c: &mut Criterion) {
    let store = build_graph(500);
    c.bench_function("graph_pagerank_default", |bencher| {
        bencher.iter(|| {
            black_box(
                PageRank::new(GRAPH)
                    .execute(black_box(&store))
                    .expect("PageRank benchmark")
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
                    .expect("PageRank benchmark")
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
                    .expect("PageRank benchmark")
                    .inner()
                    .len(),
            )
        });
    });
    c.bench_function("graph_hits_default", |bencher| {
        bencher.iter(|| {
            black_box(
                HITS::new(GRAPH)
                    .execute(black_box(&store))
                    .expect("HITS benchmark")
                    .inner()
                    .len(),
            )
        });
    });
    c.bench_function("graph_hits_low_iterations", |bencher| {
        bencher.iter(|| {
            black_box(
                HITS::new(GRAPH)
                    .max_iterations(10)
                    .execute(black_box(&store))
                    .expect("HITS benchmark")
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
                    .expect("betweenness benchmark")
                    .inner()
                    .len(),
            )
        });
    });
}

fn bench_weighted_path(c: &mut Criterion) {
    let graph_store = Arc::new(build_graph(500));
    let expr = uqa_graph::parse_rpq("knows{2,4}").expect("bounded rpq");
    let weighted = WeightedPathQueryOperator::new(expr.clone(), Arc::clone(&graph_store), GRAPH)
        .from_vertex(1)
        .with_score(0.8);
    let weighted_predicate = WeightedPathQueryOperator::new(expr, Arc::clone(&graph_store), GRAPH)
        .from_vertex(1)
        .with_max_hops(4)
        .with_predicate(|weight| weight >= 3.0, 0.5);
    let ctx = ExecutionContext::new();
    c.bench_function("graph_weighted_path_sum", |bencher| {
        bencher.iter(|| {
            black_box(
                weighted
                    .execute(black_box(&ctx))
                    .expect("weighted path benchmark should execute")
                    .len(),
            )
        });
    });
    let max_store = build_graph(500);
    c.bench_function("graph_weighted_path_max", |bencher| {
        bencher.iter(|| black_box(weighted_two_hop_max(black_box(&max_store), black_box(1)).len()));
    });
    c.bench_function("graph_weighted_path_with_predicate", |bencher| {
        bencher.iter(|| {
            black_box(
                weighted_predicate
                    .execute(black_box(&ctx))
                    .expect("weighted path predicate benchmark should execute")
                    .len(),
            )
        });
    });
}

fn bench_named_graph_creation(c: &mut Criterion) {
    c.bench_function("named_graph_create_100_graphs", |bencher| {
        bencher.iter(|| {
            let mut g = MemoryGraphStore::new();
            for i in 0..100 {
                g.create_graph(&format!("g{i}"));
            }
            black_box(g.graph_names().len())
        });
    });
}

fn bench_named_graph_traversal(c: &mut Criterion, store: &MemoryGraphStore) {
    c.bench_function("named_graph_traverse_1hop", |bencher| {
        bencher.iter(|| {
            black_box(direct_bfs(
                black_box(store),
                black_box(1),
                black_box(1),
                Some("next"),
                GRAPH,
            ))
        });
    });
    c.bench_function("named_graph_traverse_3hop", |bencher| {
        bencher.iter(|| {
            black_box(direct_bfs(
                black_box(store),
                black_box(1),
                black_box(3),
                Some("next"),
                GRAPH,
            ))
        });
    });
    c.bench_function("named_graph_traverse_operator_1hop", |bencher| {
        bencher.iter(|| {
            let result = Traverse::new(1, GRAPH)
                .label("next")
                .max_hops(1)
                .execute(black_box(store));
            black_box(result.expect("operator traversal benchmark").inner().len())
        });
    });
    c.bench_function("named_graph_traverse_operator_3hop", |bencher| {
        bencher.iter(|| {
            let result = Traverse::new(1, GRAPH)
                .label("next")
                .max_hops(3)
                .execute(black_box(store));
            black_box(result.expect("operator traversal benchmark").inner().len())
        });
    });
}

fn bench_named_graph_pattern_and_rpq(c: &mut Criterion, store: &MemoryGraphStore) {
    let mut triangle_store = store.clone();
    triangle_store
        .add_edge(Edge::new(2_000, 1, 3, "next"), GRAPH)
        .unwrap();
    triangle_store
        .add_edge(Edge::new(2_001, 3, 1, "next"), GRAPH)
        .unwrap();
    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a"))
        .add_vertex(VertexPattern::new("b"))
        .add_vertex(VertexPattern::new("c"))
        .add_edge(EdgePattern::new("a", "b").with_label("next"))
        .add_edge(EdgePattern::new("b", "c").with_label("next"))
        .add_edge(EdgePattern::new("c", "a").with_label("next"));
    c.bench_function("named_graph_triangle_pattern", |bencher| {
        bencher.iter(|| {
            let result =
                GMatch::new(black_box(pattern.clone()), GRAPH).execute(black_box(&triangle_store));
            black_box(result.expect("pattern benchmark").inner().len())
        });
    });
    c.bench_function("named_graph_rpq_kleene", |bencher| {
        let expr = uqa_graph::parse_rpq("next*").expect("rpq");
        bencher.iter(|| {
            let result = RegularPathQuery::new(black_box(expr.clone()), GRAPH)
                .from_vertex(1)
                .execute(black_box(store));
            black_box(result.expect("RPQ benchmark").inner().len())
        });
    });
}

fn bench_named_graph_set_operations(c: &mut Criterion, overlapping: &MemoryGraphStore) {
    c.bench_function("named_graph_union_graphs", |bencher| {
        bencher.iter_batched(
            || overlapping.clone(),
            |mut graph| {
                graph.union_graphs("alpha", "beta", "unioned").unwrap();
                black_box(graph.vertices_in_graph("unioned").unwrap().len())
            },
            BatchSize::SmallInput,
        );
    });
    c.bench_function("named_graph_intersect_graphs", |bencher| {
        bencher.iter_batched(
            || overlapping.clone(),
            |mut graph| {
                graph
                    .intersect_graphs("alpha", "beta", "intersected")
                    .unwrap();
                black_box(graph.vertices_in_graph("intersected").unwrap().len())
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_named_graph_property_index(c: &mut Criterion, store: &MemoryGraphStore) {
    c.bench_function("named_graph_property_index_build", |bencher| {
        bencher.iter(|| {
            let index = VertexPropertyIndex::build(black_box(store), GRAPH, &["val"])
                .expect("property index build");
            black_box(index.has_property("val"))
        });
    });
    let property_index =
        VertexPropertyIndex::build(store, GRAPH, &["val"]).expect("property index build");
    c.bench_function("named_graph_property_index_lookup", |bencher| {
        bencher.iter(|| {
            let mut matches = 0;
            for id in 1..=1_000 {
                matches += property_index
                    .lookup_eq("val", &Value::Int(id))
                    .map_or(0, BTreeSet::len);
            }
            black_box(matches)
        });
    });
}

fn bench_named_graph_isolation(c: &mut Criterion, overlapping: &MemoryGraphStore) {
    c.bench_function("named_graph_isolation", |bencher| {
        let traversal = Traverse::new(1, "alpha").label("link").max_hops(500);
        let expected = traversal
            .execute(overlapping)
            .expect("isolated traversal")
            .inner()
            .len();
        assert_eq!(expected, 500);
        bencher.iter(|| {
            let result = traversal.execute(black_box(overlapping));
            black_box(result.expect("isolated traversal").inner().len())
        });
    });
}

fn bench_named_graphs(c: &mut Criterion) {
    bench_named_graph_creation(c);
    let store = build_named_chain(1_000);
    bench_named_graph_traversal(c, &store);
    bench_named_graph_pattern_and_rpq(c, &store);
    bench_named_graph_property_index(c, &store);
    let overlapping = build_overlapping_graphs();
    bench_named_graph_set_operations(c, &overlapping);
    bench_named_graph_isolation(c, &overlapping);
}

fn bench_incremental_match(c: &mut Criterion) {
    c.bench_function("graph_incremental_add_edge", |bencher| {
        let mut store = build_graph(200);
        let pattern = person_knows_pattern();
        let mut matcher = IncrementalPatternMatcher::new(pattern, GRAPH);
        matcher.seed(&store).unwrap();
        store.add_vertex(Vertex::new(500, "Person"), GRAPH).unwrap();
        store
            .add_edge(Edge::new(10_000, 200, 500, "knows"), GRAPH)
            .unwrap();
        let mut delta = GraphDelta::new();
        delta.add_vertex(Vertex::new(500, "Person"));
        delta.add_edge(Edge::new(10_000, 200, 500, "knows"));
        bencher.iter(|| {
            let result = matcher.update(&store, &delta);
            black_box(result.expect("incremental match benchmark").len())
        });
    });

    c.bench_function("graph_incremental_remove_vertex", |bencher| {
        let mut store = build_graph(200);
        let pattern = person_knows_pattern();
        let mut matcher = IncrementalPatternMatcher::new(pattern, GRAPH);
        matcher.seed(&store).unwrap();
        store.remove_vertex(2, GRAPH).unwrap();
        let mut delta = GraphDelta::new();
        delta.remove_vertex(2);
        bencher.iter(|| {
            let result = matcher.update(&store, &delta);
            black_box(result.expect("incremental match benchmark").len())
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
    bench_centrality,
    bench_weighted_path,
    bench_named_graphs,
    bench_incremental_match
);
criterion_main!(benches);
