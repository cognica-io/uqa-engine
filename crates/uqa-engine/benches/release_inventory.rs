//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Link the release workloads once and select one suite per process.

// These modules retain their standalone Cargo benchmark entry points.
#[path = "query_matrix.rs"]
mod query_matrix;
#[expect(
    dead_code,
    reason = "the module retains its standalone benchmark entry point"
)]
#[path = "retrieval_workloads.rs"]
mod retrieval_workloads;
#[path = "sql_sqlite_e2e.rs"]
mod sql_sqlite_e2e;

fn main() {
    match std::env::var("UQA_RELEASE_BENCH_SUITE").as_deref() {
        Ok("query_matrix") => query_matrix::benches(),
        Ok("sql_sqlite_e2e") => sql_sqlite_e2e::benches(),
        Ok("retrieval_workloads") => retrieval_workloads::retrieval_benches(),
        selection => panic!("invalid UQA_RELEASE_BENCH_SUITE: {selection:?}"),
    }
    criterion::Criterion::default()
        .configure_from_args()
        .final_summary();
}
