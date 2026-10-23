//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement keeps the output metadata produced by its ordered input analysis.

use super::*;

#[test]
fn cte_analysis_distinguishes_enclosing_names_from_recursive_self_bindings() {
    for sql in [
        "WITH q AS (SELECT 7 AS id) SELECT s.id FROM (WITH q AS (SELECT id FROM q) SELECT * FROM q) s",
        "WITH q AS (SELECT 7 AS id) SELECT * FROM (WITH r AS (SELECT id FROM q), q AS (SELECT 9 AS id) SELECT r.id, q.id FROM r, q) s",
        "WITH items AS (SELECT id FROM items) SELECT * FROM items",
        "WITH RECURSIVE q(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM q WHERE id<3) SELECT * FROM q",
        "WITH RECURSIVE r AS (SELECT id FROM q), q AS (SELECT 7 AS id) SELECT * FROM r",
    ] {
        analyze(&Scopes::default(), sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn invisible_cte_diagnostics_preserve_postgresql_detail_and_hint() {
    for (sql, name) in [
        ("WITH q AS (SELECT id FROM q) SELECT * FROM q", "q"),
        (
            "WITH r AS (SELECT id FROM q), q AS (SELECT 9 AS id) SELECT * FROM r",
            "q",
        ),
        (
            "WITH \"Q\" AS (SELECT id FROM \"Q\") SELECT * FROM \"Q\"",
            "Q",
        ),
        (
            "WITH r AS (SELECT id FROM \"Q\"), \"Q\" AS (SELECT 9 AS id) SELECT * FROM r",
            "Q",
        ),
    ] {
        let error = analyze(&Scopes::default(), sql).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P01"), "{sql}");
        assert_eq!(
            error.to_string(),
            format!("relation \"{name}\" does not exist"),
            "{sql}"
        );
        assert_eq!(error.detail(), Some(format!("There is a WITH item named \"{name}\", but it cannot be referenced from this part of the query.").as_str()), "{sql}");
        assert_eq!(
            error.hint(),
            Some("Use WITH RECURSIVE, or re-order the WITH items to remove forward references."),
            "{sql}"
        );
    }
}

#[test]
fn nested_output_descriptors_reuse_analysis_and_do_not_enter_stored_syntax() {
    for sql in [
        "SELECT count(*) FROM (SELECT id AS x FROM items WHERE true UNION ALL SELECT 2) q",
        "SELECT count(*) FROM (SELECT NULL AS x WHERE true UNION ALL SELECT 'x') q",
        "SELECT count(*) FROM (SELECT 1::numeric(5,1) AS x WHERE true UNION ALL SELECT 2::numeric(10,2)) q",
    ] {
        let scopes = Scopes::default();
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        crate::binding::preparation::BLOCK_ANALYSES.with(|count| count.set(0));
        analyze_executable_plan(
            &StatementAnalysisContext {
                scopes: &scopes,
                routines: &NoRoutines,
                aliases: &NoRoutines,
            },
            &mut plan,
            &[],
        )
        .unwrap();
        assert_eq!(
            crate::binding::preparation::BLOCK_ANALYSES.with(std::cell::Cell::get),
            3,
            "{sql}"
        );
        let mut descriptors = Vec::new();
        plan.visit_queries_mut(&mut |query| {
            descriptors.push(query.analyzed_output.clone().expect("analyzed query"));
        });
        assert_eq!(descriptors.len(), 4, "{sql}");
        for descriptor in &descriptors[1..3] {
            assert_eq!(descriptor.columns(), &["x"], "{sql}");
        }
        let oid = |descriptor: &RowSchema| {
            crate::catalog::type_metadata::pg_type_oid(descriptor.column_type(0).unwrap())
        };
        assert_eq!(oid(&descriptors[1]), oid(&descriptors[2]), "{sql}");
        assert_eq!(oid(&descriptors[1]), oid(&descriptors[3]), "{sql}");
        let mut cloned = plan.clone();
        let mut index = 0;
        cloned.visit_queries_mut(&mut |query| {
            assert!(query
                .analyzed_output
                .as_ref()
                .unwrap()
                .shares_index(&descriptors[index]));
            index += 1;
        });
        let mut restored: UnifiedPlan =
            serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        restored.visit_queries_mut(&mut |query| assert!(query.analyzed_output.is_none()));
    }
}

#[test]
fn analyzed_join_sources_share_input_layouts_without_persisting_namespaces() {
    for sql in [
        "SELECT joined.a FROM ((SELECT id,1/0 AS z FROM items) q JOIN (VALUES(1)) r(id) USING(id)) joined(a,b)",
        "SELECT count(q.id) FROM (VALUES(1)) r(n) CROSS JOIN LATERAL (SELECT r.n AS id,1/0 AS z) q",
    ] {
        let scopes = Scopes::default();
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        analyze_executable_plan(
            &StatementAnalysisContext {
                scopes: &scopes,
                routines: &NoRoutines,
                aliases: &NoRoutines,
            },
            &mut plan,
            &[],
        )
        .unwrap();
        let UnifiedPlan::Query(query) = &plan else {
            panic!("query");
        };
        let source = query.analyzed_source.as_ref().expect("analyzed source");
        let children = source.children.as_ref().expect("join inputs");
        assert!(!children.0.schema.is_empty());
        assert!(!children.1.schema.is_empty());
        let UnifiedPlan::Query(cloned) = plan.clone() else {
            panic!("cloned query");
        };
        assert!(std::sync::Arc::ptr_eq(
            source,
            cloned.analyzed_source.as_ref().unwrap()
        ));
        let mut roots = 0;
        let mut plan = plan.clone();
        plan.visit_query_roots_mut(&mut |_| roots += 1);
        assert_eq!(roots, 1);
        let mut restored: UnifiedPlan = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        restored.visit_queries_mut(&mut |query| assert!(query.analyzed_source.is_none()));
    }
}

#[test]
fn executable_analysis_derives_each_query_result_once() {
    for sql in [
        "SELECT id FROM items",
        "SELECT id::numeric(12, 2), 'text', NULL FROM items",
        "CREATE VIEW saved AS SELECT id FROM items",
        "CREATE TABLE saved AS SELECT id FROM items",
        "CREATE MATERIALIZED VIEW saved AS SELECT id FROM items",
        "DECLARE saved CURSOR FOR SELECT id FROM items",
        "EXPLAIN SELECT id FROM items",
    ] {
        crate::binding::preparation::BLOCK_ANALYSES.with(|count| count.set(0));
        analyze(&Scopes::default(), sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
        let analyses = crate::binding::preparation::BLOCK_ANALYSES.with(std::cell::Cell::get);
        assert_eq!(analyses, 1, "{sql}");
    }
}

#[test]
fn executable_analysis_retains_postgresql_result_metadata() {
    // PostgreSQL 18.4's psql \gdesc supplies these names and declared types, including unknown targets converted to text.
    for (sql, columns, types) in [
        (
            "SELECT id::numeric(12,2) AS price, 'v' AS label, NULL AS unknown FROM items",
            vec!["price", "label", "unknown"],
            vec!["numeric(12,2)", "text", "text"],
        ),
        (
            "VALUES (1, 'v'), (2, NULL)",
            vec!["column1", "column2"],
            vec!["integer", "text"],
        ),
        (
            "WITH changed AS (UPDATE items SET id = id + 1 RETURNING id) SELECT id, 'x' AS label FROM changed",
            vec!["id", "label"],
            vec!["integer", "text"],
        ),
        (
            "UPDATE items SET id = 3 RETURNING id, 'changed' AS label",
            vec!["id", "label"],
            vec!["integer", "text"],
        ),
        (
            "SELECT id AS same, id::bigint AS same FROM items",
            vec!["same", "same"],
            vec!["integer", "bigint"],
        ),
    ] {
        let scopes = Scopes::default();
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let result = analyze_executable_plan(
            &StatementAnalysisContext {
                scopes: &scopes,
                routines: &NoRoutines,
                aliases: &NoRoutines,
            },
            &mut plan,
            &[],
        )
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let AnalyzedResult::Schema(schema) = result else {
            panic!("missing output schema: {sql}");
        };
        assert_eq!(schema.columns(), columns, "{sql}");
        let expected = types
            .iter()
            .map(|ty| Some(ColumnType::from_sql_name(ty).unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(schema.column_types(), expected, "{sql}");
        for (position, ty) in expected.iter().enumerate() {
            assert_eq!(
                schema.column_collation(position),
                Some(if matches!(ty, Some(ColumnType::Text)) { 100 } else { 0 }),
                "{sql}: output {position}"
            );
        }
    }
}

#[test]
fn executable_analysis_retains_parameter_identity_and_anonymous_record_fields() {
    let scopes = Scopes::default();
    let mut plan = UnifiedPlan::lower(
        crate::compile("SELECT $1 AS value, ROW(id, 'v'::text) AS r FROM items")
            .unwrap()
            .remove(0),
    );
    let result = analyze_executable_plan(
        &StatementAnalysisContext {
            scopes: &scopes,
            routines: &NoRoutines,
            aliases: &NoRoutines,
        },
        &mut plan,
        &[SQLParam::typed_scalar(Value::Int(7), ColumnType::Integer)],
    )
    .unwrap();
    assert_eq!(
        result.column_types().unwrap(),
        [Some(ColumnType::Integer), Some(ColumnType::Record)]
    );
    assert_eq!(
        result.record_fields(1).unwrap().as_ref(),
        [Some(ColumnType::Integer), Some(ColumnType::Text)]
    );
}
