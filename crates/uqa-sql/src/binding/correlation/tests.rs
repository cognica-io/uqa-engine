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
    let scope = source_scope(context, &source, &BTreeMap::new()).unwrap();
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
