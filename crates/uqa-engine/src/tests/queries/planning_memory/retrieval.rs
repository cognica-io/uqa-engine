//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_operators::OperatorTree;
use uqa_planner::retrieval_planning::query_optimizer;

fn term(query: impl Into<String>) -> OperatorTree {
    OperatorTree::Term {
        query: query.into(),
        field: Some("body".into()),
        scoring: None,
        top_k: None,
    }
}

fn populate(engine: &Engine) {
    engine.sql("CREATE TABLE retrieval_stats(id integer PRIMARY KEY, body text); CREATE INDEX retrieval_stats_body ON retrieval_stats USING gin(body); INSERT INTO retrieval_stats VALUES (1, 'known token')", &[]).unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_retrieval_statistics_retain_the_request_reservation(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("retained-stats.db"));
    populate(&engine);
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let before = memory.used();
    let optimizer = query_optimizer(&engine, "retrieval_stats", &term("known")).unwrap();
    assert_eq!(optimizer.index_stats.doc_freq("body", "known"), 1);
    assert!(
        memory.used() > before,
        "native statistics must retain their construction lease: {provider:?}"
    );
    drop(optimizer);
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_retrieval_statistics_reject_growth_and_release_partial_storage(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("limited-stats.db"));
    populate(&engine);
    drop(query_optimizer(&engine, "retrieval_stats", &term("known")).unwrap());
    let tree = OperatorTree::Union((0..4096).map(|i| term(format!("word{i}"))).collect());
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let before = memory.used();
    let sibling = memory.reserve(memory.available() - 16 * 1024).unwrap();
    let Err(error) = query_optimizer(&engine, "retrieval_stats", &tree) else {
        panic!("statistics escaped the invoking allowance: {provider:?}");
    };
    assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    assert_eq!(memory.used(), before + sibling.bytes());
    drop(sibling);
    let recovered = query_optimizer(&engine, "retrieval_stats", &tree).unwrap();
    assert_eq!(recovered.index_stats.total_docs, 1);
    assert_eq!(recovered.index_stats.doc_freq("body", "word4095"), 0);
    drop(recovered);
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
}
