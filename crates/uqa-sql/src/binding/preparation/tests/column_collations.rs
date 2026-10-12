//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared output metadata remains independent of the presence of a result row.

use super::*;

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    collations: Vec<u32>,
}

#[test]
fn prepared_outputs_retain_direct_and_explicit_collations_after_aliasing() {
    let crate::Statement::CreateTable(mut table) =
        crate::compile("CREATE TABLE t (c text, p text, n integer)")
            .unwrap()
            .remove(0)
    else {
        panic!("table declaration")
    };
    table.columns[0].collation_oid = Some(950);
    table.columns[1].collation_oid = Some(951);
    let table = crate::binding::fixture::table_definition(table.columns);
    let binding = BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([
            (RelationIdentity::new("public", "local"), table.clone()),
            (RelationIdentity::new("public", "remote"), table),
        ])),
        ..crate::binding::fixture::empty_binding_context()
    };
    let references: BTreeMap<String, Vec<Reference>> =
        serde_json::from_str(include_str!("../../tests/query_collations.json")).unwrap();
    for case in references.values().flatten() {
        let sql = &case.sql;
        let expected = &case.collations;
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let mut preparation = Preparation {
            routines: &NoRoutines,
            scope: SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        let output = preparation
            .query(&query, None)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(output.len(), expected.len(), "{sql}");
        for (position, oid) in expected.iter().enumerate() {
            assert_eq!(output.column_collation(position), Some(*oid), "{sql}");
        }
    }
}
