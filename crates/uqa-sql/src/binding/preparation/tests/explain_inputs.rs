//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn explained_inputs_match_postgresql_analysis_order_and_parameter_types() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/prepared_explain_inputs.expected.json"
    )))
    .unwrap();
    let crate::Statement::CreateTable(table) =
        crate::compile(reference["declaration"].as_str().unwrap())
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    let mut context = assignment_context();
    context.catalog = crate::binding::fixture::catalog(BTreeMap::from([(
        RelationIdentity::new("public", "explain_inputs"),
        crate::binding::fixture::table_definition(table.columns),
    )]));
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let declared =
            crate::prepared::declared_parameter_types(&NoRoutines, &mut plan, &[]).unwrap();
        let result = read_prepared_inputs(&NoRoutines, &mut plan, &declared, &context, None);
        let expected = &case["result"];
        if expected["status"] == 1 {
            let analysis = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
            let parameters = analysis
                .parameter_types
                .iter()
                .map(|ty| crate::catalog::type_metadata::pg_type_oid(ty.as_ref().unwrap()))
                .collect::<Vec<_>>();
            assert_eq!(
                serde_json::json!(parameters),
                expected["parameters"],
                "{sql}"
            );
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), expected["sqlstate"].as_str(), "{sql}");
            assert_eq!(
                error.to_string(),
                expected["message"].as_str().unwrap(),
                "{sql}"
            );
        }
    }
}
