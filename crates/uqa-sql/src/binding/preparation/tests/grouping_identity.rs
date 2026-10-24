//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn distinct_grouping_keeps_analyzed_identity_and_original_stored_syntax() {
    let crate::Statement::CreateTable(table) =
        crate::compile("CREATE TABLE grouping_inputs(region text,n integer)")
            .unwrap()
            .remove(0)
    else {
        panic!("table");
    };
    let context = BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "grouping_inputs"),
            crate::binding::fixture::table_definition(table.columns),
        )])),
        ..crate::binding::fixture::empty_binding_context()
    };
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("pg18_grouping_identity.json")).unwrap();
    let mut checked = 0;
    for case in reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["owner"] == true)
    {
        assert!(case["error"].is_null());
        let expected = case["results"][0]["rows"].as_array().unwrap().len();
        let sql = case["sql"].as_str().unwrap();
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        for _ in 0..2 {
            read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None).unwrap();
            let UnifiedPlan::Query(query) = &plan else {
                panic!("query");
            };
            let crate::plan::RelationalPlan::QueryBlock(original) = &query.root else {
                panic!("block");
            };
            assert!(
                original.group_distinct,
                "{sql}: stored syntax must retain DISTINCT"
            );
            let mut executable = original.clone();
            query
                .analyzed_grouping
                .clone()
                .expect("analyzed identity")
                .apply(&mut executable)
                .unwrap();
            assert_eq!(executable.grouping_sets.len(), expected, "{sql}");
            assert!(!executable.group_distinct);
            plan = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
            let UnifiedPlan::Query(query) = &plan else {
                panic!("query");
            };
            assert!(
                query.analyzed_grouping.is_none(),
                "{sql}: transient identity escaped serialization"
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 6);
}
