//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation qualification through retrieval support. The ASCII fixture's affected rows and lock-wait rechecks were independently checked against `PostgreSQL` 18.6 using simple tsvector matching in Docker.

use rstest::rstest;
use std::{
    path::Path,
    sync::{mpsc, Arc},
    time::Duration,
};
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLParam;

fn open(path: &Path, backend: &str) -> Engine {
    match backend {
        "memory" => Engine::new(),
        "native" => Engine::open(path).unwrap(),
        "kv" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        "redb" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

fn seed(engine: &Engine) {
    engine.sql("CREATE TABLE docs(id integer PRIMARY KEY, body text, hits integer); CREATE INDEX docs_body_gin ON docs USING gin(body); INSERT INTO docs VALUES(1,'alpha one',10),(2,'beta two',20),(3,'alpha three',30)", &[]).unwrap();
}

fn pairs(result: uqa_engine::SQLResult) -> Vec<(i64, i64)> {
    let mut pairs = result
        .rows
        .into_iter()
        .map(|row| {
            let (Value::Int(id), Value::Int(hits)) = (&row["id"], &row["hits"]) else {
                panic!("{row:?}")
            };
            (*id, *hits)
        })
        .collect::<Vec<_>>();
    pairs.sort_unstable();
    pairs
}

#[rstest]
#[case("text_match(d.body, $1)", vec![(1,11),(3,31)])]
#[case("text_match(d.body, $1) AND d.hits > 15", vec![(3,31)])]
#[case("text_match(d.body, $1) OR d.id = 2", vec![(1,11),(2,21),(3,31)])]
#[case("NOT text_match(d.body, $1)", vec![(2,21)])]
#[case("text_match(d.body, $1) AND random() >= 0", vec![(1,11),(3,31)])]
fn updates_qualify_retrieval_support_and_scalar_conditions(
    #[values("memory", "native", "kv", "redb")] backend: &str,
    #[case] predicate: &str,
    #[case] expected: Vec<(i64, i64)>,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("retrieval.db"), backend);
    seed(&engine);
    let result = engine
        .sql(
            &format!("UPDATE docs d SET hits=hits+1 WHERE {predicate} RETURNING id,hits"),
            &[SQLParam::Scalar(Value::Str("alpha".into()))],
        )
        .unwrap();
    assert_eq!(pairs(result), expected);
}

#[rstest]
fn retrieval_updates_keep_the_cte_and_subquery_scope(
    #[values("memory", "native", "kv", "redb")] backend: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("scope.db"), backend);
    seed(&engine);
    let result = engine.sql("WITH wanted AS (SELECT 3 AS id) UPDATE docs SET hits=hits+1 WHERE text_match(body,'alpha') AND id IN (SELECT id FROM wanted) RETURNING id,hits", &[]).unwrap();
    assert_eq!(pairs(result), vec![(3, 31)]);
}

#[rstest]
fn retrieval_input_survives_nested_writes_until_tuple_conflict_check(
    #[values("memory", "native", "kv", "redb")] backend: &str,
    #[values(
        "CASE WHEN id=1 THEN disturb(3) ELSE true END AND text_match(body,'alpha')",
        "text_match(body,'alpha') AND CASE WHEN id=1 THEN disturb(3) ELSE true END"
    )]
    predicate: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("nested.db"), backend);
    seed(&engine);
    engine.sql("CREATE FUNCTION disturb(k integer) RETURNS boolean VOLATILE LANGUAGE sql AS $$ UPDATE docs SET body='beta changed' WHERE id=k; SELECT true $$", &[]).unwrap();
    let error = engine
        .sql(
            &format!("UPDATE docs SET hits=hits+1 WHERE {predicate}"),
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("27000"));
    assert_eq!(
        pairs(
            engine
                .sql(
                    "SELECT id,hits FROM docs WHERE text_match(body,'alpha')",
                    &[]
                )
                .unwrap()
        ),
        vec![(1, 10), (3, 30)]
    );
}

#[test]
fn retrieval_qualification_preserves_scalar_jsonpath_dispatch() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE jdocs(id integer PRIMARY KEY,data jsonb); INSERT INTO jdocs VALUES(1,'{\"a\":1}'::jsonb),(2,'{\"a\":2}'::jsonb)", &[]).unwrap();
    let result = engine
        .sql(
            "UPDATE jdocs SET id=id+10 WHERE data @@ '$.a == 2' RETURNING id",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(12));
}

#[rstest]
fn retrieval_update_rechecks_a_document_changed_during_lock_wait(
    #[values("native", "kv", "redb")] backend: &str,
    #[values("'beta replacement'", "'alpha replacement'")] replacement: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = open(&directory.path().join("waiting.db"), backend);
    seed(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder
        .sql("BEGIN; SELECT id FROM docs WHERE id=1 FOR UPDATE", &[])
        .unwrap();
    let (send, receive) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        send.send(waiter.sql(
            "UPDATE docs SET hits=hits+1 WHERE text_match(body,'alpha') RETURNING id,hits",
            &[],
        ))
        .unwrap();
    });
    assert!(receive.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql(
            &format!("UPDATE docs SET body={replacement}, hits=90 WHERE id=1; COMMIT"),
            &[],
        )
        .unwrap();
    let result = receive
        .recv_timeout(crate::waits::COMPLETION)
        .unwrap()
        .unwrap();
    thread.join().unwrap();
    let expected = if replacement.contains("alpha") {
        vec![(1, 91), (3, 31)]
    } else {
        vec![(3, 31)]
    };
    assert_eq!(pairs(result), expected);
}
