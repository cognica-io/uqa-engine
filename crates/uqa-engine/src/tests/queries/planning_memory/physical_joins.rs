//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::query::sources::build_join_operator_with_ctes;
use uqa_sql::plan::RelationalPlan;

#[rstest::rstest]
#[case::memory_nested(Provider::Memory, false)]
#[case::memory_hash(Provider::Memory, true)]
#[case::sqlite_native_nested(Provider::SQLiteNative, false)]
#[case::sqlite_native_hash(Provider::SQLiteNative, true)]
#[case::sqlite_kv_nested(Provider::SQLiteKeyValue, false)]
#[case::sqlite_kv_hash(Provider::SQLiteKeyValue, true)]
#[case::redb_nested(Provider::Redb, false)]
#[case::redb_hash(Provider::Redb, true)]
fn native_join_predicates_admit_before_copying_and_retain_until_operator_drop(
    #[case] provider: Provider,
    #[case] equijoin: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("physical-join-owners.db"));
    let payload = "x".repeat(4 << 20);
    let equality = if equijoin { "l.id=r.id AND " } else { "" };
    let sql = format!("SELECT l.id,r.id FROM (VALUES(1)) AS l(id) JOIN (VALUES(1)) AS r(id) ON {equality}length('{payload}'::text)>0");
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let source = block.from.as_ref().unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let context = engine.source_execution_context();
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(build_join_operator_with_ctes(
            &context,
            source,
            &[],
            &mut scope,
            None,
            None,
        ));
    });
    assert!(
        allocation.bytes_total < 256 << 10,
        "{provider:?} equijoin={equijoin}: {allocation:?}"
    );
    assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        assert_eq!(
            build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), baseline);
        token.reset();
    }
    let mut operator =
        build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None).unwrap();
    assert!(
        memory.used() >= baseline + payload.len(),
        "{provider:?}: predicate payload lost its owner"
    );
    operator.open().unwrap();
    let batch = operator.next().unwrap().unwrap();
    assert_eq!(batch.rows.len(), 1);
    operator.close().unwrap();
    drop((batch, operator));
    assert_eq!(memory.used(), baseline);
    drop(scope);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

// Independent PostgreSQL 18.4 output is retained in tests/parity/pg18/join_planning_owners.expected.txt.
#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_join_planning_preserves_pg18_keys_types_lateral_patterns_and_errors(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("join-observation.db"));
    for (sql, expected) in [
        ("SELECT coalesce(l.id::text,'_') || ':' || coalesce(r.id::text,'_') AS result FROM (VALUES(1,NULL::text),(2,'b'),(3,'c')) AS l(id,label) FULL JOIN (VALUES(1,NULL::text),(2,'b'),(4,'d')) AS r(id,label) ON ROW(r.id,r.label)=ROW(l.id,l.label) ORDER BY l.id NULLS LAST,r.id NULLS LAST", &["1:_","2:2","3:_","_:1","_:4"][..]),
        ("SELECT id::text || ':' || pg_typeof(id)::text AS result FROM (VALUES(1::integer),(2)) AS l(id) FULL JOIN (VALUES(2::bigint),(3)) AS r(id) USING(id) ORDER BY id", &["1:bigint","2:bigint","3:bigint"][..]),
        ("SELECT l.id::text || ':' || coalesce(r.n::text,'_') AS result FROM (VALUES(1),(2)) AS l(id) LEFT JOIN LATERAL (SELECT l.id+1 AS n) AS r ON r.n>2 ORDER BY l.id", &["1:_","2:3"][..]),
        ("SELECT l.v AS result FROM (VALUES('a_x'),('abx')) AS l(v) JOIN (VALUES('a%')) AS r(p) ON l.v LIKE r.p AND l.v LIKE 'a#_%' ESCAPE '#' ORDER BY l.v", &["a_x"][..]),
        ("SELECT l.id::text AS result FROM (VALUES(1),(2),(3)) AS l(id) JOIN (VALUES(1),(2),(3)) AS r(id) ON l.id=r.id AND l.id=ANY(ARRAY[1,NULL,3]) ORDER BY l.id", &["1","3"][..]),
        ("SELECT coalesce(l.d::text,'_') || ':' || coalesce(r.t::date::text,'_') AS result FROM (VALUES(DATE '2024-01-01'),(DATE '2024-01-02')) AS l(d) FULL JOIN (VALUES(TIMESTAMP '2024-01-02 00:00:00'),(TIMESTAMP '2024-01-03 00:00:00')) AS r(t) ON l.d=r.t ORDER BY l.d NULLS LAST,r.t NULLS LAST", &["2024-01-01:_","2024-01-02:2024-01-02","_:2024-01-03"][..]),
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        let actual = result.rows.iter().map(|row| match &row["result"] { Value::Str(value) => value.as_str(), value => panic!("unexpected result {value:?}") }).collect::<Vec<_>>();
        assert_eq!(actual, expected, "{provider:?}: {sql}");
    }
    for (sql, state) in [
        ("SELECT l.id FROM (VALUES(1)) AS l(id) FULL JOIN (VALUES(2)) AS r(id) ON l.id>r.id", "0A000"),
        ("SELECT l.id FROM (VALUES(32768)) AS l(id) JOIN (VALUES(1::smallint)) AS r(id) ON l.id::smallint=r.id", "22003"),
    ] {
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some(state), "{provider:?}: {sql}");
        assert_eq!(engine.sql("SELECT 1 AS id", &[]).unwrap().rows[0]["id"], Value::Int(1));
    }
    engine.close().unwrap();
}

// Independent PostgreSQL 18.4 output is retained in tests/parity/pg18/nullable_join_planning.expected.txt.
#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_nullable_join_metadata_preserves_whole_rows_aliases_and_lateral_presence(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("nullable-join-observation.db"));
    for (sql, expected) in [
        ("SELECT l.id::text || ':' || coalesce(r::text,'ABSENT') AS result FROM (VALUES(1),(2)) AS l(id) LEFT JOIN (VALUES(NULL::integer,NULL::text)) AS r(n,v) ON l.id=1 ORDER BY l.id;", &["1:(,)","2:ABSENT"][..]),
        ("SELECT a.id::text || ':' || coalesce(b::text,'ABSENT') || ':' || coalesce(c::text,'ABSENT') AS result FROM (VALUES(1),(2),(3)) AS a(id) LEFT JOIN (VALUES(2)) AS b(id) ON a.id=b.id LEFT JOIN (VALUES(NULL::integer)) AS c(n) ON a.id<>3 ORDER BY a.id;", &["1:ABSENT:()","2:(2):()","3:ABSENT:ABSENT"][..]),
        ("SELECT id::text || ':' || coalesce(l::text,'ABSENT') || ':' || coalesce(r::text,'ABSENT') AS result FROM (VALUES(1::integer),(2)) AS l(id) FULL JOIN (VALUES(2::bigint),(3)) AS r(id) USING(id) ORDER BY id;", &["1:(1):ABSENT","2:(2):(2)","3:ABSENT:(3)"][..]),
        ("SELECT j.x::text || ':' || j::text AS result FROM ((VALUES(1),(2)) AS l(id) LEFT JOIN (VALUES(1,NULL::text)) AS r(id,v) ON l.id=r.id) AS j(x,y,z) ORDER BY j.x;", &["1:(1,1,)","2:(2,,)"][..]),
        ("SELECT l.id::text || ':' || coalesce(r::text,'ABSENT') AS result FROM (VALUES(1),(2)) AS l(id) LEFT JOIN LATERAL (SELECT NULL::integer AS n WHERE l.id=1) AS r ON true ORDER BY l.id;", &["1:()","2:ABSENT"][..]),
        ("SELECT l.id::text || ':' || coalesce(r::text,'ABSENT') AS result FROM (VALUES(1),(2)) AS l(id) LEFT JOIN LATERAL (SELECT NULL::integer AS n) AS r ON l.id=1 ORDER BY l.id;", &["1:()","2:ABSENT"][..]),
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        let actual = result.rows.iter().map(|row| match &row["result"] {
            Value::Str(value) => value.as_str(), value => panic!("unexpected result {value:?}")
        }).collect::<Vec<_>>();
        assert_eq!(actual, expected, "{provider:?}: {sql}");
    }
    engine.close().unwrap();
}

// Independent PostgreSQL 18.4 output is retained in tests/parity/pg18/join_output_planning.expected.txt.
#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_join_output_preserves_using_coercions_namespaces_and_lateral_aliases(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("join-output-observation.db"));
    for (sql, expected) in [
        ("SELECT merged.id::text || ':' || pg_typeof(merged.id)::text || ':' || coalesce(l.id::text,'_') || ':' || coalesce(r.id::text,'_') AS result FROM (VALUES(1::integer),(2)) AS l(id) FULL JOIN (VALUES(2::bigint),(3)) AS r(id) USING(id) AS merged ORDER BY merged.id;", &["1:bigint:1:_","2:bigint:2:2","3:bigint:_:3"][..]),
        ("SELECT id::text || ':' || pg_typeof(id)::text || ':' || label || ':' || extra AS result FROM (VALUES(1::smallint,'a'),(2,'b')) AS l(id,label) NATURAL RIGHT JOIN (VALUES(2::bigint,'x'),(3,'y')) AS r(id,extra) WHERE label IS NOT NULL ORDER BY id;", &["2:bigint:b:x"][..]),
        ("SELECT j::text || ':' || pg_typeof(j.k)::text AS result FROM ((VALUES(1::integer,'a'),(2,'b')) AS l(id,label) FULL JOIN (VALUES(2::bigint,'x'),(3,'y')) AS r(id,extra) USING(id)) AS j(k,v,w) ORDER BY j.k;", &["(1,a,):bigint","(2,b,x):bigint","(3,,y):bigint"][..]),
        ("SELECT j.x::text || ':' || j.y::text AS result FROM ((VALUES(2),(1)) AS l(id) CROSS JOIN (VALUES(7)) AS r(id)) AS j(x,y) ORDER BY j.x;", &["1:7","2:7"][..]),
        ("SELECT merged.id::text || ':' || r.n::text AS result FROM (VALUES(1::integer),(2)) AS l(id) LEFT JOIN LATERAL (SELECT l.id::bigint AS id,l.id+10 AS n) AS r USING(id) AS merged ORDER BY merged.id;", &["1:11","2:12"][..]),
        ("SELECT k::text || ':' || coalesce(j::text,'ABSENT') AS result FROM ((VALUES(1::integer,'a'),(2,'b')) AS l(id,label) FULL JOIN (VALUES(2::bigint,'x'),(3,'y')) AS r(id,extra) USING(id)) AS j(k,v,w) FULL JOIN (VALUES(2::bigint),(4)) AS t(k) USING(k) ORDER BY k;", &["1:(1,a,)","2:(2,b,x)","3:(3,,y)","4:ABSENT"][..]),
        ("SELECT ROW(j.*)::text AS result FROM ((VALUES(1::integer,'a'),(2,'b')) AS l(id,label) FULL JOIN (VALUES(2::bigint,'x'),(3,'y')) AS r(id,extra) USING(id)) AS j(k,v,w) ORDER BY j.k;", &["(1,a,)","(2,b,x)","(3,,y)"][..]),
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        let actual = result.rows.iter().map(|row| match &row["result"] {
            Value::Str(value) => value.as_str(), value => panic!("unexpected result {value:?}"),
        }).collect::<Vec<_>>();
        assert_eq!(actual, expected, "{provider:?}: {sql}");
    }
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_join_output_admits_aliases_and_retains_the_last_batch_owner(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("join-output-owners.db"));
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(uqa_sql::compile("SELECT * FROM ((VALUES(1,'a'),(2,'b')) AS l(id,v) FULL JOIN (VALUES(2::bigint,'x'),(3,'y')) AS r(id,w) USING(id)) AS j(k,v,w)").unwrap().remove(0)) else { panic!("query") };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let source = block.from.as_mut().unwrap();
    let uqa_sql::plan::SourcePlan::Join { column_aliases, .. } = source else {
        panic!("join")
    };
    let payload = "label".repeat(1 << 18);
    column_aliases[0] = payload.clone();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let context = engine.source_execution_context();
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let measured = allocation_counter::measure(|| {
        result = Some(build_join_operator_with_ctes(
            &context,
            source,
            &[],
            &mut scope,
            None,
            None,
        ));
    });
    assert!(
        measured.bytes_total < 256 << 10,
        "{provider:?}: {measured:?}"
    );
    assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        assert_eq!(
            build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), baseline);
        token.reset();
    }
    let mut operator =
        build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None).unwrap();
    assert!(memory.used() >= baseline + payload.len());
    operator.open().unwrap();
    let batch = operator.next().unwrap().unwrap();
    assert_eq!(batch.schema.columns()[0], payload);
    assert_eq!(batch.rows.len(), 3);
    operator.close().unwrap();
    drop(operator);
    assert!(memory.used() >= baseline + payload.len());
    drop(batch);
    assert_eq!(memory.used(), baseline);
    drop(scope);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
