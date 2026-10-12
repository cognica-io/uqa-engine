//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::relational::values::test_support::Services;
use uqa_sql::plan::{RelationalPlan, UnifiedPlan};

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    collations: Option<Vec<u32>>,
}

#[test]
fn values_from_sources_preserve_postgresql_collations_through_qualification() {
    let cases: Vec<Reference> = serde_json::from_str(include_str!(
        "../../relational/values/tests/collations.json"
    ))
    .unwrap();
    assert_eq!(
        cases
            .iter()
            .filter(|case| case.collations.is_some())
            .count(),
        4
    );
    for case in cases {
        let Some(expected) = case.collations else {
            continue;
        };
        let UnifiedPlan::Query(plan) =
            UnifiedPlan::lower(uqa_sql::compile(&case.sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::Values { rows, subqueries } = plan.root else {
            panic!("VALUES")
        };
        assert!(subqueries.is_empty());
        for alias in [None, Some("v".to_string())] {
            let source = SourcePlan::Values {
                rows: rows.clone(),
                alias: alias.clone(),
                column_aliases: alias.as_ref().map(|_| vec!["x".into()]).unwrap_or_default(),
                internal_relation: None,
                internal_column_types: Vec::new(),
                internal_column_collations: Vec::new(),
            };
            let mut operator = build_values_source(
                &Services,
                &Services,
                &source,
                &[],
                &crate::catalog::test_support::empty_scope(),
                || Arc::new(Services),
                None,
            )
            .unwrap();
            let name = if alias.is_some() { "x" } else { "column1" };
            for (position, expected) in expected.iter().enumerate() {
                assert_eq!(
                    operator.row_schema().column_collation(position),
                    Some(*expected)
                );
                assert_eq!(operator.row_schema().collation_of(name), Some(*expected));
                if alias.is_some() {
                    assert_eq!(
                        operator.row_schema().qualified_collation("v", name),
                        Some(*expected)
                    );
                }
            }
            let (_, actual) = crate::physical::run_to_rows(operator.as_mut()).unwrap();
            assert_eq!(actual.len(), rows.len());
            let values = actual
                .into_iter()
                .map(|row| vec![row[name].clone()])
                .collect::<Vec<_>>();
            crate::query::relational::values::test_support::assert_values(&case.sql, &values);
        }
    }
}
