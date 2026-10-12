//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declaration metadata must reach source binding before expression analysis.

use super::*;
use crate::binding::{self, fixture};

#[test]
fn declared_collations_follow_native_foreign_and_bound_source_aliases() {
    let mut columns = vec![
        column("c", ColumnType::Text),
        column("p", ColumnType::Text),
        column("n", ColumnType::Integer),
    ];
    columns[0].attribute_number = Some(1);
    columns[1].attribute_number = Some(4);
    columns[0].collation_oid = Some(950);
    columns[1].collation_oid = Some(951);
    let table = fixture::table_definition(columns);
    let context = binding::BindingContext {
        catalog: fixture::catalog_with_foreign_tables(
            BTreeMap::from([(RelationIdentity::new("public", "local"), table.clone())]),
            BTreeMap::from([(RelationIdentity::new("public", "remote"), table)]),
        ),
        ..fixture::empty_binding_context()
    };
    for table in ["local", "remote"] {
        for bound in [false, true] {
            let mut source = SourcePlan::Table {
                name: table.into(),
                qualifier: table.into(),
                alias: Some("q".into()),
                column_aliases: vec!["first".into(), "second".into(), "number".into()],
                bound_columns: bound.then(|| vec!["p".into(), "c".into(), "n".into()]),
                include_descendants: true,
            };
            let bound_schema = binding::bind_source_plan_schema(
                &EmptyRoutineResolution,
                &source,
                &[],
                &context,
                None,
            )
            .unwrap();
            let analyzed = binding::analyze_source_plan_schema(
                &EmptyRoutineResolution,
                &source,
                &[],
                &context,
                None,
            )
            .unwrap();
            let executing = binding::bind_source_plan_schema_for_execution(
                &EmptyRoutineResolution,
                &mut source,
                &[],
                &context,
                None,
            )
            .unwrap();
            for schema in [bound_schema, analyzed, executing] {
                let expected = if bound { [951, 950, 0] } else { [950, 951, 0] };
                for (position, oid) in expected.into_iter().enumerate() {
                    assert_eq!(schema.column_collation(position), Some(oid), "{table}");
                }
                assert_eq!(schema.qualified_collation("q", "first"), Some(expected[0]));
                assert_eq!(schema.collation_of("second"), Some(expected[1]));
                assert_eq!(schema.columns(), ["first", "second", "number"]);
                assert_eq!(schema.physical_slot(1), Some(1));
            }
        }
    }
}

#[test]
fn operator_join_inputs_retain_each_relation_declaration() {
    let mut left = column("value", ColumnType::Text);
    left.collation_oid = Some(950);
    let mut right = left.clone();
    right.collation_oid = Some(951);
    let catalog = fixture::catalog(BTreeMap::from([
        (
            RelationIdentity::new("public", "a"),
            fixture::table_definition(vec![left]),
        ),
        (
            RelationIdentity::new("public", "b"),
            fixture::table_definition(vec![right]),
        ),
    ]));
    let resolution = fixture::resolution(vec!["public".into()], "pg_temp_fixture".into());
    let (left, right) = binding::catalog_sources::operator_join_relation_schemas(
        &catalog,
        &resolution,
        Some(&crate::ast::OperatorJoinRelations {
            left: "a".into(),
            right: "b".into(),
        }),
    )
    .unwrap();
    assert_eq!(left.qualified_collation("a", "value"), Some(950));
    assert_eq!(right.qualified_collation("b", "value"), Some(951));
}
