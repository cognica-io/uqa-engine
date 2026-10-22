//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn implicit_action_inputs_preserve_their_enclosing_row_identity() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = CorrelationContext {
        routines: &crate::binding::fixture::NoRoutines,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: None,
    };
    for (sql, expected) in [
        ("SELECT merge_action()", true),
        ("SELECT lower(merge_action())", true),
        ("SELECT (SELECT merge_action())", true),
        ("SELECT public.merge_action()", false),
        ("SELECT \"merge_action\"()", false),
        ("SELECT lower('constant')", false),
    ] {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        assert_eq!(
            query_depends_on_outer_row(context, &query).unwrap(),
            expected,
            "{sql}"
        );
        let mut enclosing = QueryScope::default();
        enclosing
            .internal_relations
            .insert(crate::semantics::merge_action_attribute().relation());
        assert!(
            !query_has_external_reference(context, &query, &mut vec![enclosing]).unwrap(),
            "{sql}"
        );
    }
}

#[test]
fn table_range_aliases_replace_physical_correlation_names() {
    let crate::ast::Statement::CreateTable(definition) =
        crate::compile("CREATE TABLE correlation_alias_source(id INTEGER, label TEXT)")
            .unwrap()
            .remove(0)
    else {
        panic!("expected table definition")
    };
    let catalog = crate::binding::fixture::catalog(BTreeMap::from([(
        crate::RelationIdentity::from_legacy_name("public.correlation_alias_source").unwrap(),
        crate::binding::fixture::table_definition(definition.columns),
    )]));
    let resolution = crate::binding::fixture::resolution(vec!["public".into()], "pg_temp_1".into());
    let context = CorrelationContext {
        routines: &crate::binding::fixture::NoRoutines,
        catalog: catalog.as_ref(),
        resolution: &resolution,
        scoped_relations: None,
    };
    let source = SourcePlan::Table {
        executable_query: None,
        bound_columns: None,
        name: "correlation_alias_source".into(),
        qualifier: "correlation_alias_source".into(),
        alias: Some("source".into()),
        column_aliases: vec!["key".into(), "value".into()],
        include_descendants: true,
    };
    let scope = source_scope(context, &source, &BTreeMap::new(), &[]).unwrap();
    assert_eq!(
        scope.columns.names,
        BTreeSet::from(["key".to_string(), "value".to_string()])
    );
}

struct ScopedColumns(BTreeMap<String, Vec<String>>);

impl CorrelationRelations for ScopedColumns {
    fn columns(&self, relation: &str) -> Option<&[String]> {
        self.0.get(relation).map(Vec::as_slice)
    }
}

#[test]
fn correlation_uses_scoped_relation_descriptors_without_hiding_outer_columns() {
    let binding = crate::binding::fixture::empty_binding_context();
    let relations = ScopedColumns(BTreeMap::from([("new_rows".into(), vec!["id".into()])]));
    let context = CorrelationContext {
        routines: &crate::binding::fixture::NoRoutines,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: Some(&relations),
    };
    for (sql, correlated) in [
        ("SELECT sum(id) FROM new_rows", false),
        ("SELECT sum(n.id) FROM new_rows AS n", false),
        ("SELECT sum(key) FROM new_rows AS n(key)", false),
        ("SELECT sum(outer_value) FROM new_rows", true),
        ("SELECT sum(parent.id) FROM new_rows", true),
        ("SELECT sum(id) FROM public.new_rows", true),
        (
            "WITH new_rows(local_id) AS (VALUES (1)) SELECT sum(local_id) FROM new_rows",
            false,
        ),
        (
            "WITH \"T\"(x) AS (VALUES (1)), t(y) AS (VALUES (2)) SELECT x FROM t",
            true,
        ),
        (
            "WITH \"T\"(x) AS (VALUES (1)), t(y) AS (VALUES (2)) SELECT y FROM t",
            false,
        ),
        (
            "WITH \"T\"(x) AS (VALUES (1)), t(y) AS (VALUES (2)) SELECT x FROM \"T\"",
            false,
        ),
    ] {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        assert_eq!(
            query_depends_on_outer_row(context, &query).unwrap(),
            correlated,
            "{sql}"
        );
    }
}

#[test]
fn lateral_source_dependencies_distinguish_caller_from_local_and_enclosing_names() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = CorrelationContext {
        routines: &crate::binding::fixture::NoRoutines,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: None,
    };
    let current = crate::RowSchema::with_qualified_types(
        "t",
        vec!["a".into()],
        vec![Some(crate::ColumnType::Integer)],
    );
    let outer = crate::RowSchema::with_qualified_types(
        "ancestor",
        vec!["n".into()],
        vec![Some(crate::ColumnType::Integer)],
    );
    let input = crate::RowSchema::with_outer_schema(&current, &outer);
    for (source, expected) in [
        ("generate_series(1,3) g(x)", false),
        ("LATERAL generate_series(1,3) g(x)", false),
        ("generate_series(t.a,3) g(x)", true),
        ("generate_series(a,3) g(x)", true),
        ("generate_series((SELECT t.a),3) g(x)", true),
        (
            "generate_series((SELECT a FROM (VALUES(2)) v(a)),3) g(x)",
            false,
        ),
        ("generate_series(ancestor.n,3) g(x)", false),
        ("generate_series((SELECT ancestor.n),3) g(x)", false),
        (
            "ROWS FROM(generate_series(1,2),generate_series(t.a,3)) g(x,y)",
            true,
        ),
        (
            "ROWS FROM(generate_series(1,2),generate_series(2,3)) g(x,y)",
            false,
        ),
        ("LATERAL (VALUES(t.a)) v(x)", true),
        ("LATERAL (VALUES(ancestor.n)) v(x)", false),
        ("LATERAL (SELECT t.a) v", true),
        ("LATERAL (SELECT t.a FROM (VALUES(2)) t(a)) v", false),
        ("LATERAL (SELECT t.a FROM (VALUES(2)) \"T\"(a)) v", true),
        ("LATERAL (SELECT a FROM (VALUES(2)) t(\"A\")) v", true),
        (
            "LATERAL (SELECT sum(t.a) FROM generate_series(t.a,t.a+1) t(a)) v",
            true,
        ),
        (
            "LATERAL (WITH local_cte AS (SELECT 1 AS x) SELECT (SELECT x FROM local_cte)) v",
            false,
        ),
        (
            "LATERAL (WITH local_cte AS (SELECT t.a AS x) SELECT (SELECT x FROM local_cte)) v",
            true,
        ),
    ] {
        let sql = format!("SELECT * FROM (VALUES(1)) t(a), {source}");
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
        else {
            panic!("query");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block");
        };
        let Some(SourcePlan::Join { right, .. }) = &block.from else {
            panic!("join");
        };
        assert_eq!(
            source_depends_on_current_row(context, right, &block.subqueries, &input).unwrap(),
            expected,
            "{sql}"
        );
    }
}

fn verify_independent_join_inputs(ids: &[&str]) {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = CorrelationContext {
        routines: &crate::binding::fixture::NoRoutines,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: None,
    };
    let input = crate::RowSchema::with_qualified_types(
        "a",
        vec!["n".into()],
        vec![Some(crate::ColumnType::Integer)],
    );
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/lateral_functions_oracle.expected.json"
    ))
    .unwrap();
    let mut checked = 0;
    for case in reference["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        if !ids.contains(&id) {
            continue;
        }
        assert!(case["error"].is_null(), "{id}");
        assert_eq!(
            case["results"][0]["rows"].as_array().unwrap().len(),
            1,
            "{id}"
        );
        assert_eq!(case["results"][0]["rows"][0][2], "1", "{id}");
        for dependent in [false, true] {
            let sql = case["sql"].as_str().unwrap();
            let sql = if dependent {
                sql.replace("nextval('descriptor_calls')", "a.n")
            } else {
                sql.to_owned()
            };
            let crate::plan::UnifiedPlan::Query(query) =
                crate::plan::UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
            else {
                panic!("query");
            };
            let RelationalPlan::QueryBlock(block) = &query.root else {
                panic!("block");
            };
            let Some(SourcePlan::Join { right, .. }) = &block.from else {
                panic!("join");
            };
            assert_eq!(
                source_depends_on_current_row(context, right, &block.subqueries, &input).unwrap(),
                dependent,
                "{sql}",
            );
        }
        checked += 1;
    }
    assert_eq!(checked, ids.len());
}

#[test]
fn declared_source_columns_preserve_independent_join_inputs() {
    verify_independent_join_inputs(&[
        "descriptor_json",
        "descriptor_ordinality",
        "descriptor_values",
        "descriptor_derived",
        "descriptor_rows_from",
        "descriptor_join_alias",
    ]);
}

#[test]
fn wildcard_sources_preserve_independent_join_inputs() {
    verify_independent_join_inputs(&[
        "wildcard_star",
        "wildcard_qualified_star",
        "wildcard_cte_star",
        "wildcard_cte_partial_alias",
        "wildcard_using_alias",
        "wildcard_using_qualified_star",
        "wildcard_nested_outer_star",
        "wildcard_nested_shadow_star",
        "wildcard_recursive_cte_star",
        "wildcard_using_partial_alias",
    ]);
}
