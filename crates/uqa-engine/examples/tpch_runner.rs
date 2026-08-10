//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Machine-readable UQA side of the `PostgreSQL` 17 TPC-H comparison.

use std::time::Instant;

use serde::Serialize;
use uqa_core::Value;

#[path = "../tests/support/tpch_fixture.rs"]
#[allow(dead_code)]
mod tpch_fixture;

#[derive(Serialize)]
struct QueryRun {
    query: usize,
    result: tpch_fixture::CanonicalResult,
    elapsed_ms: Vec<f64>,
}

#[derive(Serialize)]
struct Report {
    engine: &'static str,
    scale_factor: f64,
    load_ms: f64,
    iterations: usize,
    queries: Vec<QueryRun>,
}

struct Args {
    iterations: usize,
    query: Option<usize>,
    explain: bool,
}

fn parse_args() -> Args {
    let mut iterations = 3;
    let mut query = None;
    let mut explain = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--iterations" => {
                iterations = arguments
                    .next()
                    .expect("--iterations requires a value")
                    .parse()
                    .expect("--iterations must be an integer");
            }
            "--query" => {
                query = Some(
                    arguments
                        .next()
                        .expect("--query requires a value")
                        .parse()
                        .expect("--query must be an integer"),
                );
            }
            "--explain" => explain = true,
            unknown => panic!("unknown argument: {unknown}"),
        }
    }
    assert!(iterations > 0, "--iterations must be positive");
    assert!(
        query.is_none_or(|number| (1..=22).contains(&number)),
        "--query must be in 1..=22"
    );
    Args {
        iterations,
        query,
        explain,
    }
}

fn main() {
    let args = parse_args();

    let load_started = Instant::now();
    let engine = tpch_fixture::load_engine();
    let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
    let mut runs = Vec::new();
    for (index, query) in tpch_fixture::load_queries().iter().enumerate() {
        let query_number = index + 1;
        if args.query.is_some_and(|selected| selected != query_number) {
            continue;
        }
        if args.explain {
            let explained = engine
                .sql(&format!("EXPLAIN {query}"), &[])
                .unwrap_or_else(|error| panic!("TPC-H Q{query_number:02} EXPLAIN: {error}"));
            eprintln!("TPC-H Q{query_number:02} plan:");
            for row in explained.rows {
                match row.get("plan").expect("EXPLAIN plan column") {
                    Value::Str(plan) | Value::FixedChar(plan) => eprintln!("{plan}"),
                    value => panic!("EXPLAIN returned a non-text plan row: {value:?}"),
                }
            }
        }
        let result = engine
            .sql(query, &[])
            .unwrap_or_else(|error| panic!("TPC-H Q{query_number:02}: {error}"));
        let result = tpch_fixture::canonical_result(&result);
        let mut elapsed_ms = Vec::with_capacity(args.iterations);
        for _ in 0..args.iterations {
            let started = Instant::now();
            let measured = engine
                .sql(query, &[])
                .unwrap_or_else(|error| panic!("TPC-H Q{query_number:02}: {error}"));
            elapsed_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(
                tpch_fixture::canonical_result(&measured),
                result,
                "TPC-H Q{query_number:02} changed across measured iterations"
            );
        }
        runs.push(QueryRun {
            query: query_number,
            result,
            elapsed_ms,
        });
    }
    let report = Report {
        engine: "uqa",
        scale_factor: 0.001,
        load_ms,
        iterations: args.iterations,
        queries: runs,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize TPC-H report")
    );
}
