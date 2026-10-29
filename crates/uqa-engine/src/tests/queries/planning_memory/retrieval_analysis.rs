//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_operators::OperatorTree;
use uqa_planner::retrieval_planning::{query_optimizer, RetrievalPlanningCatalog};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_text_analysis_and_top_k_share_the_request_allowance(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("text-analysis-admission.db"));
    engine.sql("CREATE TABLE text_analysis(id integer PRIMARY KEY, body text); CREATE INDEX text_analysis_body ON text_analysis USING gin(body); INSERT INTO text_analysis VALUES (1, 'known token')", &[]).unwrap();
    engine
        .sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    let query = "known ".repeat(2048);
    let tree = |field: Option<&str>| OperatorTree::Term {
        query: query.clone(),
        field: field.map(str::to_owned),
        scoring: None,
        top_k: None,
    };
    uqa_planner::statement_planning::PlannerStatisticsCatalog::column_statistics(
        &engine,
        "text_analysis",
    )
    .unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    for field in [Some("body"), None] {
        let tree = tree(field);
        let before = memory.used();
        let occupied = memory.reserve(memory.available() - 1024).unwrap();
        let allocation = allocation_counter::measure(|| {
            let Err(error) = query_optimizer(&engine, "text_analysis", &tree) else {
                panic!("{provider:?}: query analysis escaped its allowance");
            };
            assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
        });
        assert!(
            allocation.bytes_total < 64 * 1024,
            "{provider:?}: {allocation:?}"
        );
        assert_eq!(memory.used(), before + occupied.bytes());
        drop(occupied);
        let recovered = query_optimizer(&engine, "text_analysis", &tree).unwrap();
        assert_eq!(
            recovered
                .index_stats
                .doc_freq(field.unwrap_or("_default"), &query),
            1
        );
        drop(recovered);
        assert_eq!(memory.used(), before);
    }
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - 1024).unwrap();
    let allocation = allocation_counter::measure(|| {
        let error = RetrievalPlanningCatalog::text_top_k_capabilities(
            &engine,
            "text_analysis",
            "body",
            &query,
        )
        .expect_err("top-k analysis must reject before allocation");
        assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    });
    assert!(
        allocation.bytes_total < 64 * 1024,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    let recovered =
        RetrievalPlanningCatalog::text_top_k_capabilities(&engine, "text_analysis", "body", &query)
            .unwrap();
    assert_eq!(recovered.analyzed_term_count, 2048);
    assert_eq!(recovered.indexed_document_count, 1);
    assert_eq!(memory.used(), before);
    engine.sql("COMMIT", &[]).unwrap();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
